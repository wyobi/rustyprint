'use strict'

// Runs renders on worker threads, at most `concurrency` at once. A render never holds the event
// loop, nor libuv's thread pool, which file system, DNS, crypto and zlib calls share: a native
// call made with koffi's `.async` would hold one of its four threads for the whole render.

const path = require('node:path')
const { Worker } = require('node:worker_threads')
const { errorFor } = require('./errors')
const { libraryPath } = require('./library')

const WORKER = path.join(__dirname, 'worker.js')

class Pool {
  constructor (concurrency) {
    if (!Number.isInteger(concurrency) || concurrency < 1) {
      throw new RangeError(`concurrency must be a positive integer, not ${concurrency}`)
    }
    this.concurrency = concurrency
    this.workers = new Set()
    this.idle = []
    this.running = new Map()
    // The workers kept for a record set each, which take no other work until released.
    this.reserved = new Set()
    // The record sets that keep a worker, to give up when the pool closes.
    this.sets = new Set()
    this.queue = []
    this.closed = false
  }

  // Renders a document on a worker, or with a `window` measures the width of its content in a
  // window of that size. A signal stops a render that has not started; one that has started
  // runs to the end, bounded by the deadline in its options.
  run (html, options, signal, window) {
    return this.enqueue({ message: { html, options, window } }, signal)
  }

  // Keeps a worker for a record set, whose fonts and native set live on one thread: resolves to
  // the worker once one is free, which then does only what call() sends it, until release().
  // A signal stops the wait for a worker.
  reserve (signal) {
    return this.enqueue({ reserve: true }, signal)
  }

  // Has a reserved worker do one call of its record set.
  call (worker, set) {
    return new Promise((resolve, reject) => {
      if (!this.workers.has(worker)) return reject(new Error('the render worker of the record set has stopped'))
      this.post(worker, { message: { set }, resolve, reject })
    })
  }

  // Gives a reserved worker back, for the renders that wait.
  release (worker) {
    if (!this.reserved.delete(worker) || !this.workers.has(worker)) return
    this.idle.push(worker)
    this.drain()
  }

  enqueue (work, signal) {
    return new Promise((resolve, reject) => {
      if (this.closed) return reject(closedError())
      if (signal?.aborted) return reject(signal.reason)

      const task = { ...work, resolve, reject, signal, onAbort: null }
      if (signal) {
        task.onAbort = () => {
          const index = this.queue.indexOf(task)
          if (index >= 0) {
            this.queue.splice(index, 1)
            reject(signal.reason)
          }
        }
        signal.addEventListener('abort', task.onAbort, { once: true })
      }
      this.queue.push(task)
      this.drain()
    })
  }

  // Stops every worker, and refuses the renders still queued and any made later. Record sets
  // that are open are given up first, so that each native set is freed on its own thread.
  async close () {
    this.closed = true
    for (const task of this.queue.splice(0)) task.reject(closedError())
    await Promise.all([...this.sets].map(set => set.abort()))
    await Promise.all([...this.workers].map(worker => worker.terminate()))
  }

  drain () {
    while (this.queue.length > 0) {
      let worker = this.idle.pop()
      if (!worker && this.workers.size < this.concurrency) {
        try {
          worker = this.spawn()
        } catch (error) {
          // No native library to render with: every render waiting fails the same way.
          for (const task of this.queue.splice(0)) {
            task.signal?.removeEventListener('abort', task.onAbort)
            task.reject(error)
          }
          return
        }
      }
      if (!worker) return
      this.start(worker, this.queue.shift())
    }
  }

  spawn () {
    const worker = new Worker(WORKER, { workerData: { library: libraryPath() } })
    this.workers.add(worker)
    worker.on('message', reply => this.finish(worker, reply))
    worker.on('error', error => this.fail(worker, error))
    worker.on('exit', code => this.fail(worker, new Error(`a render worker stopped with code ${code}`)))
    return worker
  }

  start (worker, task) {
    task.signal?.removeEventListener('abort', task.onAbort)
    if (task.reserve) {
      this.reserved.add(worker)
      worker.unref()
      task.resolve(worker)
    } else {
      this.post(worker, task)
    }
  }

  post (worker, task) {
    try {
      worker.postMessage(task.message)
    } catch (error) {
      // Options or assets that cannot be copied to another thread.
      this.rest(worker)
      task.reject(error)
      return
    }
    this.running.set(worker, task)
    // A worker keeps the process alive only while it renders.
    worker.ref()
  }

  // A worker with nothing to do is free for the next render, unless a record set keeps it.
  rest (worker) {
    worker.unref()
    if (!this.reserved.has(worker)) this.idle.push(worker)
  }

  finish (worker, reply) {
    const task = this.running.get(worker)
    this.running.delete(worker)
    this.rest(worker)

    const { error, width, pdf, ...rest } = reply
    if (error) task.reject(errorFor(error.kind, error.message))
    else if (width !== undefined) task.resolve(width)
    // The bytes moved from the worker, as a Buffer over them.
    else task.resolve(pdf ? { pdf: Buffer.from(pdf), ...rest } : rest)
    this.drain()
  }

  fail (worker, error) {
    if (!this.workers.delete(worker)) return
    this.idle = this.idle.filter(other => other !== worker)
    this.reserved.delete(worker)
    const task = this.running.get(worker)
    this.running.delete(worker)
    task?.reject(error)
    if (!this.closed) this.drain()
  }
}

function closedError () {
  return new Error('the renderer is closed')
}

module.exports = { Pool }
