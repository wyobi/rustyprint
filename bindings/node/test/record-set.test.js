'use strict'

const assert = require('node:assert/strict')
const crypto = require('node:crypto')
const { setTimeout: sleep } = require('node:timers/promises')
const { describe, it } = require('node:test')
const v8 = require('node:v8')
const vm = require('node:vm')
const zlib = require('node:zlib')
const rustyprint = require('..')
const { pageSizes, pageTexts } = require('./helpers')

const { createRenderer, createRecordSet, toReadable, RecordSetError, LimitExceededError, InvalidOptionsError } = rustyprint

// A record of `pages` pages.
function record (name, pages = 1) {
  return Array.from({ length: pages }, (_, page) =>
    `<p style="break-before: ${page > 0 ? 'page' : 'auto'}">${name}, page ${page + 1}</p>`).join('')
}

function isPdf (pdf) {
  return pdf.subarray(0, 5).toString('latin1') === '%PDF-' && pdf.subarray(-6).toString('latin1').trim() === '%%EOF'
}

// A PNG chunk: its length, type, data and checksum.
function chunk (type, data) {
  const body = Buffer.concat([Buffer.from(type, 'latin1'), data])
  const head = Buffer.alloc(4)
  head.writeUInt32BE(data.length)
  const sum = Buffer.alloc(4)
  sum.writeUInt32BE(zlib.crc32(body))
  return Buffer.concat([head, body, sum])
}

// A PNG of random pixels, about `bytes` in size since noise does not compress, as a data URI.
function noisePng (bytes) {
  const side = Math.ceil(Math.sqrt(bytes / 3))
  const rows = Buffer.alloc(side * (side * 3 + 1))
  for (let y = 0; y < side; y++) {
    // Each row is a filter byte of zero and its pixels.
    crypto.randomFillSync(rows, y * (side * 3 + 1) + 1, side * 3)
  }
  const header = Buffer.alloc(13)
  header.writeUInt32BE(side, 0)
  header.writeUInt32BE(side, 4)
  header.set([8, 2, 0, 0, 0], 8)
  const png = Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', zlib.deflateSync(rows, { level: 1 })),
    chunk('IEND', Buffer.alloc(0))
  ])
  return `data:image/png;base64,${png.toString('base64')}`
}

// Collects garbage now, so that memory no longer in use does not count as growth.
function collectGarbage () {
  v8.setFlagsFromString('--expose-gc')
  vm.runInNewContext('gc')()
}

describe('createRecordSet', () => {
  it('prints records as one PDF with the pages of them all', async () => {
    const set = createRecordSet({
      stylesheet: '@import "page.css";',
      assets: { 'page.css': '@page { size: 300px 200px; margin: 10px }' }
    })
    const added = []
    for (const pages of [1, 3, 2]) added.push(await set.add(record(`Record of ${pages}`, pages)))
    const summary = await set.finish()

    assert.deepEqual(added.map(output => output.pages), [1, 3, 2])
    assert.deepEqual({ pages: summary.pages, records: summary.records }, { pages: 6, records: 3 })
    const pdf = Buffer.concat([...added.map(output => output.pdf), summary.pdf])
    assert.ok(isPdf(pdf))
    assert.deepEqual(pageSizes(pdf), Array(6).fill([225, 150]))
  })

  it('gives the bytes of a record when it is added, before the set is finished', async () => {
    const set = createRecordSet()
    const first = await set.add(record('First'))
    assert.ok(Buffer.isBuffer(first.pdf))
    assert.equal(first.pdf.subarray(0, 5).toString('latin1'), '%PDF-')
    assert.deepEqual(pageSizes(first.pdf), [[595.3, 841.9]])
    const second = await set.add(record('Second'))
    assert.ok(second.pdf.length > 0)
    assert.notEqual(second.pdf.subarray(0, 5).toString('latin1'), '%PDF-')
    assert.ok(!isPdf(Buffer.concat([first.pdf, second.pdf])))
    const { pdf } = await set.finish()
    assert.ok(isPdf(Buffer.concat([first.pdf, second.pdf, pdf])))
  })

  it('gives the warnings of each record', async () => {
    const set = createRecordSet()
    const first = await set.add('<style>p { colr: red }</style><p>x</p>')
    const second = await set.add('<p>x</p>')
    await set.finish()
    assert.match(first.warnings.join(), /colr/)
    assert.deepEqual(second.warnings, [])
  })

  it('finishes a set without records as a PDF without pages', async () => {
    const summary = await createRecordSet().finish()
    assert.ok(isPdf(summary.pdf))
    assert.deepEqual({ pages: summary.pages, records: summary.records }, { pages: 0, records: 0 })
  })

  it('runs the calls made at once one after another, in the order they were made', async () => {
    const set = createRecordSet()
    // Each record has its own page size, which tells the order of the pages in the file.
    const sizes = [100, 200, 300, 400, 500]
    const calls = sizes.map(size => set.add(`<style>@page { size: ${size}px 400px }</style><p>x</p>`))
    const outputs = await Promise.all([...calls, set.finish()])
    const pdf = Buffer.concat(outputs.map(output => output.pdf))
    assert.ok(isPdf(pdf))
    assert.deepEqual(pageSizes(pdf).map(([width]) => width), sizes.map(size => size * 0.75))
    assert.equal(outputs.at(-1).records, 5)
  })

  it('rejects when a record fails, and every call after it', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet({ limits: { maxPages: 1 } })
    await set.add(record('One'))
    await assert.rejects(set.add(record('Two', 2)), LimitExceededError)
    await assert.rejects(set.add(record('Three')), error =>
      error instanceof RecordSetError && error.kind === 'record_set' && /earlier call .* failed/.test(error.message))
    await assert.rejects(set.finish(), RecordSetError)
    await set.abort()
    // The set's worker, the only one, is free again.
    assert.ok(isPdf((await renderer.render('<p>x</p>')).pdf))
    await renderer.close()
  })

  it('bounds the pages of the whole set', async () => {
    const set = createRecordSet({ limits: { maxTotalPages: 2 } })
    await set.add(record('One', 2))
    await assert.rejects(set.add(record('Two')), LimitExceededError)
  })

  it('refuses invalid options at the first call', async () => {
    const set = createRecordSet({ pageSize: 'B99' })
    await assert.rejects(set.add('<p>x</p>'), InvalidOptionsError)
    await assert.rejects(set.add('<p>x</p>'), RecordSetError)
  })

  it('refuses a record that is not HTML text', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    await assert.rejects(set.add(42), TypeError)
    await assert.rejects(set.finish(), RecordSetError)
    assert.ok(isPdf((await renderer.render('<p>x</p>')).pdf))
    await renderer.close()
  })

  it('keeps one worker until it is aborted, which frees the worker', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    await set.add(record('One'))

    // The only worker belongs to the set, so a render waits.
    let rendered = false
    const render = renderer.render('<p>x</p>').then(output => {
      rendered = true
      return output
    })
    await sleep(200)
    assert.equal(rendered, false)

    await set.abort()
    assert.ok(isPdf((await render).pdf))
    await assert.rejects(set.add(record('Two')), error =>
      error instanceof RecordSetError && /aborted/.test(error.message))
    await assert.rejects(set.finish(), RecordSetError)
    // Aborting again does nothing.
    await set.abort()
    await renderer.close()
  })

  it('refuses the calls that wait when it is aborted, after the call that runs', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    const running = set.add(record('One', 50))
    const waiting = set.add(record('Two'))
    // The first call is with the worker by now.
    await new Promise(resolve => setImmediate(resolve))
    const aborted = set.abort()
    assert.equal((await running).pages, 50)
    await assert.rejects(waiting, RecordSetError)
    await aborted
    assert.ok(isPdf((await renderer.render('<p>x</p>')).pdf))
    await renderer.close()
  })

  it('stops waiting for a worker when it is aborted', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const first = renderer.createRecordSet()
    await first.add(record('One'))
    const second = renderer.createRecordSet()
    const waiting = second.add(record('Two'))
    await second.abort()
    await assert.rejects(waiting, RecordSetError)
    assert.equal((await first.finish()).records, 1)
    await renderer.close()
  })

  it('prints two sets at once, each on a worker of its own', async () => {
    const renderer = createRenderer({ concurrency: 2 })
    const sets = [renderer.createRecordSet({ pageSize: 'A5' }), renderer.createRecordSet({ pageSize: 'A4' })]
    const parts = [[], []]
    for (let i = 0; i < 4; i++) {
      // Both sets print a record at the same time.
      const outputs = await Promise.all(sets.map(set => set.add(record(`Record ${i}`, 2))))
      outputs.forEach((output, index) => parts[index].push(output.pdf))
    }
    const summaries = await Promise.all(sets.map(set => set.finish()))
    const sizes = [[419.5, 595.3], [595.3, 841.9]]
    summaries.forEach((summary, index) => {
      const pdf = Buffer.concat([...parts[index], summary.pdf])
      assert.ok(isPdf(pdf))
      assert.deepEqual(pageSizes(pdf), Array(8).fill(sizes[index]))
      assert.deepEqual({ pages: summary.pages, records: summary.records }, { pages: 8, records: 4 })
    })
    await renderer.close()
  })

  it('leaves the other workers to render while a set is open', async () => {
    const renderer = createRenderer({ concurrency: 2 })
    const set = renderer.createRecordSet()
    const first = await set.add(record('One'))
    // The set is open and keeps its worker; the other worker renders.
    const output = await renderer.render('<p>x</p>', { pageSize: 'A5' })
    assert.deepEqual(pageSizes(output.pdf), [[419.5, 595.3]])
    const second = await set.add(record('Two'))
    const summary = await set.finish()
    assert.equal(pageSizes(Buffer.concat([first.pdf, second.pdf, summary.pdf])).length, 2)
    await renderer.close()
  })

  it('is aborted when its renderer closes', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    await set.add(record('One'))
    await renderer.close()
    await assert.rejects(set.add(record('Two')), RecordSetError)
  })

  it('holds one record at a time: 200 records with an image of 1 MB each', { timeout: 300000 }, async () => {
    const records = 200
    const html = i => `<h1>Record ${i}</h1><img src="${noisePng(1024 * 1024)}" style="width: 400px">`
    const rss = () => process.memoryUsage.rss()
    const mb = value => Math.round(value / 1024 / 1024)

    // Making the records takes memory of its own, which grows for a while: make them all
    // once without printing them, so that what grows afterwards is the record set.
    let made = 0
    for (let i = 0; i < records; i++) made += html(i).length
    assert.ok(made > records * 1024 * 1024)
    collectGarbage()
    const before = rss()

    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    let bytes = 0
    let pages = 0
    let highest = before
    for (let i = 0; i < records; i++) {
      const output = await set.add(html(i))
      assert.deepEqual(output.warnings, [])
      // The image is in this record's bytes, which are dropped here.
      assert.ok(output.pdf.length > 900 * 1024, `record ${i} has ${output.pdf.length} bytes`)
      bytes += output.pdf.length
      pages += output.pages
      highest = Math.max(highest, rss())
    }
    collectGarbage()
    const after = rss()
    const summary = await set.finish()
    await renderer.close()

    console.log(`record set memory: ${records} records, ${mb(bytes)} MB of PDF written; RSS ${mb(before)} MB before, ` +
      `at most ${mb(highest - before)} MB more while printing, ${mb(after - before)} MB more after the last record`)
    assert.deepEqual({ pages: summary.pages, records: summary.records }, { pages, records })
    assert.ok(bytes > 180 * 1024 * 1024)
    // With the set's worker, its fonts and a record in flight: far less than the PDF written.
    assert.ok(highest - before < 100 * 1024 * 1024, `RSS grew ${mb(highest - before)} MB over ${records} records`)
  })
})

// A table of five columns: its rows from `from` up to `to`, as a table of its own or as more
// rows of the table before it.
function tableSlice (from, to, continues) {
  let rows = ''
  for (let row = from; row < to; row++) {
    rows += `<tr><td>${row + 1}</td><td>Customer ${row * 31 % 977}</td><td>Order ${row * 17 % 4999}</td>` +
      `<td>${row * 7 % 1000}.00</td><td>${'Delivered to the depot. '.repeat(row % 4 + 1)}</td></tr>`
  }
  return `<style>
    table { width: 100%; border-collapse: collapse }
    td, th { border: 1px solid #888; padding: 2px 6px }
    tbody tr:nth-child(even) { background: #eef }
  </style><table${continues ? ' data-rp-continue' : ''}>
  <thead><tr><th>Line</th><th>Customer</th><th>Order</th><th>Amount</th><th>Delivery</th></tr></thead>
  <tbody>${rows}</tbody></table>`
}

describe('a record that continues', () => {
  it('goes on the page of the record before it', async () => {
    const set = createRecordSet()
    const first = await set.add('<p>First</p>')
    const second = await set.add('<p>Second</p>', { continue: true })
    const third = await set.add('<p>Third</p>')
    const end = await set.finish()
    assert.deepEqual([first.pages, second.pages, third.pages, end.pages, end.records], [1, 0, 1, 2, 3])
    const pdf = Buffer.concat([first.pdf, second.pdf, third.pdf, end.pdf])
    assert.deepEqual(pageTexts(pdf), ['FirstSecond', 'Third'])
  })

  it('refuses options that are not a continue flag', async () => {
    const set = createRecordSet()
    assert.throws(() => set.add('<p>x</p>', { continue: 'yes' }), TypeError)
    assert.throws(() => set.add('<p>x</p>', true), TypeError)
    await set.abort()
  })

  it('streams a long table in slices as the PDF of the whole table', async () => {
    const rows = 1200
    const size = 150
    let next = 0
    const stream = toReadable(createRecordSet(), () => {
      if (next >= rows) return null
      const from = next
      next += size
      return { html: tableSlice(from, from + size, from > 0), continue: from > 0 }
    })
    const chunks = []
    for await (const chunk of stream) chunks.push(chunk)
    const sliced = Buffer.concat(chunks)
    assert.ok(isPdf(sliced))
    const whole = (await rustyprint.render(tableSlice(0, rows, false))).pdf

    const texts = pageTexts(sliced)
    assert.ok(texts.length > 20, `the table takes ${texts.length} pages`)
    assert.equal(pageSizes(sliced).length, pageSizes(whole).length)
    assert.deepEqual(texts, pageTexts(whole))
    // Every page has the header once, and the rows run through the slices.
    for (const text of texts) assert.equal(text.split('LineCustomerOrderAmountDelivery').length, 2)
    assert.ok(texts.at(-1).includes('1200Customer'))
  })
})

describe('toReadable', () => {
  // The records of a set, given one at a time, with a count of how many were asked for.
  function source (names) {
    const asked = { count: 0 }
    const nextHtml = async () => {
      asked.count++
      return names[asked.count - 1] === undefined ? null : record(names[asked.count - 1])
    }
    return { asked, nextHtml }
  }

  it('streams the PDF of the records', async () => {
    const { asked, nextHtml } = source(['One', 'Two', 'Three'])
    const chunks = []
    for await (const chunk of toReadable(createRecordSet(), nextHtml)) chunks.push(chunk)
    // A chunk per record, and the end of the file.
    assert.equal(chunks.length, 4)
    assert.equal(asked.count, 4)
    const pdf = Buffer.concat(chunks)
    assert.ok(isPdf(pdf))
    assert.equal(pageSizes(pdf).length, 3)
  })

  it('prints no record that the consumer has not asked for', async () => {
    const names = Array.from({ length: 12 }, (_, i) => `Record ${i}`)
    const { asked, nextHtml } = source(names)
    const stream = toReadable(createRecordSet(), nextHtml)
    const chunks = []
    stream.on('data', chunk => chunks.push(chunk))
    const ended = new Promise((resolve, reject) => stream.on('end', resolve).on('error', reject))

    // The consumer takes one chunk and stops reading.
    await new Promise(resolve => stream.once('data', resolve))
    stream.pause()
    await sleep(500)
    assert.ok(asked.count <= 2, `${asked.count} records were asked for while the consumer was paused`)
    assert.ok(chunks.length <= 2)

    stream.resume()
    await ended
    assert.equal(asked.count, names.length + 1)
    const pdf = Buffer.concat(chunks)
    assert.ok(isPdf(pdf))
    assert.equal(pageSizes(pdf).length, names.length)
  })

  it('aborts the set when the source of records fails', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    let asked = 0
    const stream = toReadable(set, async () => {
      if (++asked === 2) throw new Error('no more rows')
      return record('One')
    })
    await assert.rejects(async () => { for await (const chunk of stream) void chunk }, /no more rows/)
    await assert.rejects(set.add(record('Two')), RecordSetError)
    assert.ok(isPdf((await renderer.render('<p>x</p>')).pdf))
    await renderer.close()
  })

  it('fails when a record fails', async () => {
    const set = createRecordSet({ limits: { maxPages: 1 } })
    const records = [record('One'), record('Two', 2), record('Three')]
    const stream = toReadable(set, () => records.shift() ?? null)
    await assert.rejects(async () => { for await (const chunk of stream) void chunk }, LimitExceededError)
    assert.equal(records.length, 1)
  })

  it('aborts the set when it is destroyed', async () => {
    const renderer = createRenderer({ concurrency: 1 })
    const set = renderer.createRecordSet()
    const { asked, nextHtml } = source(['One', 'Two', 'Three'])
    const stream = toReadable(set, nextHtml)
    await new Promise(resolve => stream.once('data', resolve))
    stream.destroy()
    await new Promise(resolve => stream.once('close', resolve))
    await assert.rejects(set.finish(), RecordSetError)
    assert.ok(asked.count <= 2)
    assert.ok(isPdf((await renderer.render('<p>x</p>')).pdf))
    await renderer.close()
  })
})
