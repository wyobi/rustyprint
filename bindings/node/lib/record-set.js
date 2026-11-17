'use strict'

// A record set: many documents printed as one PDF, one record at a time, with the bytes of each
// record handed over as it is printed. One worker of the pool prints the whole set, since the
// native set, its thread and its fonts live on one thread; the other workers keep rendering.

const { Readable } = require('node:stream')
const { RecordSetError } = require('./errors')

function nothing () {}

class RecordSet {
  #pool
  #options
  // Stops the wait for a worker: the caller's signal, or abort().
  #waiting = new AbortController()
  #signal
  // The worker kept for the set, from its first call until it ends.
  #worker = null
  // Why the set takes no more calls, once it is finished, aborted or has failed.
  #ended = null
  // Settles when the calls made so far are done, so that each call runs after the one before.
  #last = Promise.resolve()
  #aborted = null

  constructor (pool, options) {
    const { signal, ...rest } = options ?? {}
    this.#pool = pool
    this.#options = rest
    this.#signal = signal ? AbortSignal.any([signal, this.#waiting.signal]) : this.#waiting.signal
  }

  // Prints a document as the next record, and resolves to the bytes of its pages, the number
  // of its pages and its warnings.
  add (html) {
    return this.#next(async () => {
      await this.#open()
      return this.#call({ op: 'add', html })
    })
  }

  // Ends the PDF, and resolves to its last bytes and the pages and records of the whole file.
  finish () {
    return this.#next(async () => {
      await this.#open()
      const summary = await this.#call({ op: 'finish' })
      this.#end('the record set is finished')
      return summary
    })
  }

  // Gives the set up: the call that is running ends first, the calls that wait are refused,
  // and the worker goes back to the pool. Does nothing to a set that has ended.
  abort () {
    this.#aborted ??= this.#abort()
    return this.#aborted
  }

  async #abort () {
    const reason = 'the record set was aborted'
    this.#ended ??= reason
    this.#waiting.abort(new RecordSetError(reason))
    await this.#last
    await this.#free()
  }

  // Runs `work` once the calls made before it are done.
  #next (work) {
    const run = this.#last.then(() => {
      if (this.#ended) throw new RecordSetError(this.#ended)
      return work()
    })
    this.#last = run.then(nothing, nothing)
    return run
  }

  // Before the first call: keeps a worker and opens the native set on it.
  async #open () {
    if (this.#worker) return
    try {
      this.#worker = await this.#pool.reserve(this.#signal)
    } catch (error) {
      this.#ended ??= `the record set did not open: ${error.message}`
      throw error
    }
    this.#pool.sets.add(this)
    await this.#call({ op: 'open', options: this.#options })
  }

  async #call (set) {
    try {
      return await this.#pool.call(this.#worker, set)
    } catch (error) {
      // A record that fails ends the native set, so the set is given up whatever failed.
      this.#ended ??= `an earlier call on the record set failed: ${error.message}`
      await this.#free()
      throw error
    }
  }

  #end (why) {
    this.#ended ??= why
    this.#release()
  }

  // Frees the native set on its worker, and gives the worker back.
  async #free () {
    if (!this.#worker) return
    await this.#pool.call(this.#worker, { op: 'abort' }).catch(nothing)
    this.#release()
  }

  #release () {
    const worker = this.#worker
    if (!worker) return
    this.#worker = null
    this.#pool.sets.delete(this)
    this.#pool.release(worker)
  }
}

// The PDF of a record set as a Readable that prints only what is read: each time the consumer
// wants more, it asks `nextHtml` for the next record, adds it and pushes its bytes; when
// `nextHtml` gives null, it finishes the set and pushes the last bytes. A consumer that stops
// reading stops the records. An error, or destroy(), aborts the set.
function toReadable (set, nextHtml, { highWaterMark = 0 } = {}) {
  let ended = false

  async function more (readable) {
    // A record always has pages to push; the loop is for a set that would give none.
    while (!readable.destroyed) {
      const html = await nextHtml()
      if (readable.destroyed) return
      if (html == null) {
        const { pdf } = await set.finish()
        ended = true
        if (pdf.length > 0) readable.push(pdf)
        readable.push(null)
        return
      }
      const { pdf } = await set.add(html)
      if (pdf.length > 0) {
        readable.push(pdf)
        return
      }
    }
  }

  return new Readable({
    highWaterMark,
    read () {
      more(this).catch(error => this.destroy(error))
    },
    destroy (error, callback) {
      if (ended) return callback(error)
      set.abort().then(() => callback(error), () => callback(error))
    }
  })
}

module.exports = { RecordSet, toReadable }
