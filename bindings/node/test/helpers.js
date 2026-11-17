'use strict'

const fs = require('node:fs')
const path = require('node:path')
const zlib = require('node:zlib')

const REPO = path.resolve(__dirname, '../../..')

// A paragraph to repeat for documents that take a while to render.
const LOREM = '<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit.</p>'

// A document long enough to be still rendering while a test checks what else runs meanwhile,
// as long as the Ruby spec's: 1.0.4 renders twice as fast, so 3000 paragraphs no longer were.
const LONG = LOREM.repeat(10000)

// The page sizes of a PDF in points, from its /MediaBox entries.
function pageSizes (pdf) {
  return [...pdf.toString('latin1').matchAll(/\/MediaBox \[0 0 ([\d.]+) ([\d.]+)\]/g)]
    .map(([, width, height]) => [round(width), round(height)])
}

function round (value) {
  return Math.round(Number(value) * 10) / 10
}

// The sample invoice and its assets.
function invoice () {
  const dir = path.join(REPO, 'demos/samples/invoice')
  return {
    html: fs.readFileSync(path.join(dir, 'index.html'), 'utf8'),
    assets: {
      'invoice.css': fs.readFileSync(path.join(dir, 'invoice.css')),
      'logo.png': fs.readFileSync(path.join(dir, 'logo.png'))
    }
  }
}

// The object `number` of a PDF that has a cross-reference table: its dictionary as text and,
// for a stream, its bytes inflated.
function pdfObject (pdf, offsets, number) {
  const start = offsets.get(number)
  const text = pdf.toString('latin1', start, pdf.indexOf('endobj', start))
  const stream = /stream\r?\n/.exec(text)
  if (!stream) return { dict: text, data: null }
  const dict = text.slice(0, stream.index)
  const from = start + stream.index + stream[0].length
  const bytes = pdf.subarray(from, from + Number(/\/Length (\d+)/.exec(dict)[1]))
  return { dict, data: /\/FlateDecode/.test(dict) ? zlib.inflateSync(bytes) : bytes }
}

// Where each object of a PDF starts, from its cross-reference table.
function pdfOffsets (pdf) {
  const text = pdf.toString('latin1')
  const table = text.slice(Number(/startxref\s+(\d+)\s+%%EOF\s*$/.exec(text)[1]))
  const offsets = new Map()
  let number = 0
  for (const line of table.split(/\r?\n/).slice(1)) {
    const section = /^(\d+) (\d+)$/.exec(line.trim())
    const entry = /^(\d{10}) \d{5} ([nf])/.exec(line)
    if (section) number = Number(section[1])
    else if (entry) {
      if (entry[2] === 'n') offsets.set(number, Number(entry[1]))
      number++
    } else if (line.startsWith('trailer')) break
  }
  return offsets
}

// What a font's character codes stand for, from its ToUnicode map.
function unicodeMap (cmap) {
  const map = new Map()
  const text = hex => String.fromCharCode(...hex.match(/.{4}/g).map(unit => parseInt(unit, 16)))
  for (const [, body] of cmap.matchAll(/beginbfchar([\s\S]*?)endbfchar/g)) {
    for (const [, code, to] of body.matchAll(/<([0-9a-f]+)>\s*<([0-9a-f]+)>/gi)) map.set(parseInt(code, 16), text(to))
  }
  for (const [, body] of cmap.matchAll(/beginbfrange([\s\S]*?)endbfrange/g)) {
    for (const [, from, to, first] of body.matchAll(/<([0-9a-f]+)>\s*<([0-9a-f]+)>\s*<([0-9a-f]+)>/gi)) {
      for (let code = parseInt(from, 16); code <= parseInt(to, 16); code++) {
        map.set(code, String.fromCodePoint(parseInt(first, 16) + code - parseInt(from, 16)))
      }
    }
  }
  return map
}

// The bytes of the literal strings of a text-showing operator's operand.
function stringBytes (operand) {
  const bytes = []
  for (const [, body] of operand.matchAll(/\(((?:\\[\s\S]|[^\\)])*)\)/g)) {
    const escapes = { n: 10, r: 13, t: 9, b: 8, f: 12 }
    for (const [, octal, escaped, plain] of body.matchAll(/\\([0-7]{1,3})|\\([\s\S])|([\s\S])/g)) {
      if (octal !== undefined) bytes.push(parseInt(octal, 8))
      else if (escaped !== undefined) bytes.push(escapes[escaped] ?? escaped.charCodeAt(0))
      else bytes.push(plain.charCodeAt(0))
    }
  }
  return bytes
}

// The text of each page of a PDF that rustyprint wrote: the runs of text it shows, in order.
function pageTexts (pdf) {
  const offsets = pdfOffsets(pdf)
  const object = number => pdfObject(pdf, offsets, number)
  const text = pdf.toString('latin1')
  const root = object(Number(/\/Root (\d+) 0 R/.exec(text.slice(text.lastIndexOf('trailer')))[1]))
  const refs = list => [...list.matchAll(/(\d+) 0 R/g)].map(([, number]) => Number(number))
  const pages = []
  const walk = number => {
    const { dict } = object(number)
    if (/\/Type \/Pages\b/.test(dict)) refs(/\/Kids \[([^\]]*)\]/.exec(dict)[1]).forEach(walk)
    else pages.push(dict)
  }
  walk(Number(/\/Pages (\d+) 0 R/.exec(root.dict)[1]))
  const maps = new Map()
  const mapOf = font => {
    if (!maps.has(font)) {
      const unicode = /\/ToUnicode (\d+) 0 R/.exec(object(font).dict)
      maps.set(font, unicodeMap(object(Number(unicode[1])).data.toString('latin1')))
    }
    return maps.get(font)
  }
  return pages.map(page => {
    const resources = /\/Resources (\d+) 0 R/.exec(page)
    const fonts = new Map([...(resources ? object(Number(resources[1])).dict : page)
      .matchAll(/\/(F\d+) (\d+) 0 R/g)].map(([, name, number]) => [name, Number(number)]))
    const content = refs(/\/Contents \[([^\]]*)\]/.exec(page)[1])
      .map(number => object(number).data.toString('latin1')).join('\n')
    const runs = []
    let map = null
    for (const [, font, shown] of content.matchAll(/\/(F\d+) [\d.]+ Tf|(\[(?:\((?:\\[\s\S]|[^\\)])*\)|[^\]])*\]) TJ/g)) {
      if (font) map = mapOf(fonts.get(font))
      else {
        const bytes = stringBytes(shown)
        let run = ''
        for (let at = 0; at + 1 < bytes.length; at += 2) run += map.get(bytes[at] * 256 + bytes[at + 1]) ?? '?'
        runs.push(run)
      }
    }
    return runs.join('')
  })
}

module.exports = { REPO, LOREM, LONG, pageSizes, pageTexts, invoice }
