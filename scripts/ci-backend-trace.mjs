import { createHash } from 'node:crypto'
import { createWriteStream, rename, writeFile } from 'node:fs'
import process from 'node:process'
import { Writable } from 'node:stream'

const EXIT_FAILURE = 125
const LOCAL_CLOSE_MS = 1000 // Taken from the existing evidence reserve, not a new job budget.

export function createPrivateTrace(file, { deadline, closeMs = LOCAL_CLOSE_MS, onFailure }) {
  const raw = createWriteStream(file, { flags: 'wx', mode: 0o600 })
  const hash = createHash('sha256')
  let receivedBytes = 0
  let writtenBytes = 0
  let fileClosed = false
  let failure = false
  let finishPromise
  let finalCallback
  const fail = () => {
    if (failure)
      return
    failure = true
    onFailure?.()
  }

  const stream = new Writable({
    write(bytes, encoding, callback) {
      receivedBytes += bytes.length
      raw.write(bytes, encoding, (error) => {
        if (error) {
          fail()
        }
        else {
          writtenBytes += bytes.length
          hash.update(bytes)
        }

        callback(error)
      })
    },
    final(callback) {
      finalCallback = callback
      if (fileClosed)
        callback(failure ? new Error('Private trace failed') : undefined)
      else
        raw.end()
    },
    destroy(error, callback) {
      raw.destroy()
      callback(error)
    },
  })
  stream.on('error', fail)
  raw.on('error', (error) => {
    fail()
    stream.destroy(error)
  })
  raw.on('close', () => {
    fileClosed = true
    finalCallback?.(failure ? new Error('Private trace failed') : undefined)
  })

  const summary = () => ({
    scope: 'received-owned-output',
    state: failure ? 'error' : fileClosed && stream.closed && stream.writableFinished ? 'closed' : 'unknown',
    complete: !failure && fileClosed && stream.closed && stream.writableFinished && receivedBytes === writtenBytes,
    receivedBytes,
    writtenBytes,
    fileClosed,
    streamClosed: stream.closed,
    sha256: hash.copy().digest('hex'),
    hashScope: !failure && fileClosed && stream.closed && stream.writableFinished ? 'complete-private-file' : 'confirmed-write-callbacks',
  })
  const bound = () => Math.max(0, Math.min(closeMs, deadline - Date.now()))
  const ready = new Promise((resolve) => {
    const timer = setTimeout(() => {
      fail()
      stream.destroy()
      resolve(false)
    }, bound())
    raw.once('open', () => {
      clearTimeout(timer)
      resolve(true)
    })
    raw.once('error', () => {
      clearTimeout(timer)
      resolve(false)
    })
  })
  const finish = () => {
    if (finishPromise)
      return finishPromise
    finishPromise = new Promise((resolve) => {
      let settled = false
      let timer
      const settle = () => {
        if (settled || !fileClosed || !stream.closed)
          return
        settled = true
        clearTimeout(timer)
        resolve(summary())
      }

      timer = setTimeout(() => {
        if (settled)
          return
        settled = true
        stream.destroy()
        raw.destroy()
        resolve(summary())
      }, bound())
      raw.once('close', settle)
      stream.once('close', settle)
      stream.end()
      settle()
    })
    return finishPromise
  }

  return { stream, ready, finish }
}

// This is a pre-exit record. Only the runner/OS can subsequently prove exit.
export async function writeLifecycle(file, value, deadline, closeMs = LOCAL_CLOSE_MS) {
  return await new Promise((resolve) => {
    let settled = false
    let timer
    const done = (state) => {
      if (settled)
        return
      settled = true
      clearTimeout(timer)
      resolve(state)
    }

    timer = setTimeout(done, Math.max(0, Math.min(closeMs, deadline - Date.now())), 'unknown')
    writeFile(`${file}.tmp`, `${JSON.stringify(value)}\n`, { mode: 0o600, flag: 'wx' }, (error) => {
      if (error || settled)
        return done('error')
      rename(`${file}.tmp`, file, error => done(error ? 'error' : 'written'))
    })
  })
}

export function exitCode(value) {
  return Number.isInteger(value) && value >= 0 && value <= 255 ? value : EXIT_FAILURE
}

export async function foregroundAction(run) {
  // Public progress is advisory. An EPIPE or stalled reader must neither stop
  // raw private capture nor retain this process after mandatory closure.
  const ignorePublicError = () => {}
  process.stdout.on('error', ignorePublicError)
  process.stderr.on('error', ignorePublicError)
  let code = EXIT_FAILURE
  try {
    const result = await run()
    const decision = typeof result === 'object' && result !== null ? result.exitDecision : undefined
    if (decision?.kind === 'signal' && decision.signal === 'SIGKILL'
      && decision.ownedClosureVerified === true && decision.publisherClosureVerified === true) {
      // This failure path cannot claim file closure or a numeric OS exit. Node
      // teardown can itself await a stuck local FS pool. Signal this PID only,
      // after verified owned-process/publisher closure; never use it for green.
      process.kill(process.pid, 'SIGKILL')
    }

    code = exitCode(decision?.kind === 'code' ? decision.code : typeof result === 'number' ? result : EXIT_FAILURE)
  }
  catch {}

  // run() has made a bounded success/failure decision about mandatory closure.
  // Discard only public progress here; this call never proves OS process exit.
  process.stdout.destroy()
  process.stderr.destroy()
  process.exit(code)
}
