'use strict'

// A render worker: renders or measures each document the pool sends on this thread, and sends
// back the PDF, moved rather than copied, the width or the error. For a record set, which the
// pool keeps this worker for, it holds the native set between the calls of the set.

const { parentPort, workerData } = require('node:worker_threads')

// The library the main thread found, so that every worker loads the same one without looking.
require('./library').useLibrary(workerData.library)
const {
  renderBytes,
  contentWidthSync,
  openRecordSet,
  addRecord,
  finishRecordSet,
  freeRecordSet
} = require('./render')

// The native record set open on this worker, if any.
let recordSet = null

// Lets go of the native record set, whose thread and fonts go with it.
function endRecordSet () {
  const set = recordSet
  recordSet = null
  if (set) freeRecordSet(set)
}

// One call of a record set: 'open', 'add', 'finish' or 'abort'.
function recordSetCall ({ op, options, html, continues }) {
  if (op === 'add' && recordSet) return addRecord(recordSet, html, continues)
  if (op === 'finish' && recordSet) {
    try {
      return finishRecordSet(recordSet)
    } finally {
      endRecordSet()
    }
  }
  // A set left open here is one whose owner has gone.
  endRecordSet()
  if (op === 'open') recordSet = openRecordSet(options)
  else if (op !== 'abort') throw new Error('this worker has no record set open')
  return {}
}

parentPort.on('message', ({ html, options, window, set }) => {
  let reply
  let transfer
  try {
    if (set) {
      reply = recordSetCall(set)
    } else if (window) {
      reply = { width: contentWidthSync(html, window, options) }
    } else {
      reply = renderBytes(html, options)
    }
    if (reply.pdf) transfer = [reply.pdf]
  } catch (error) {
    const kind = error.kind ?? (error instanceof TypeError ? 'invalid_argument' : 'internal')
    reply = { error: { kind, message: error.message } }
  }
  parentPort.postMessage(reply, transfer)
})
