import { spawn } from 'node:child_process'
import process from 'node:process'

// The worker must checkpoint this supervisor before sending "start". A broken IPC
// channel means the worker died; stop the process group before exiting ourselves.
let child: ReturnType<typeof spawn> | undefined
let stopping = false
let finished = false
let timer: NodeJS.Timeout | undefined

function stop() {
  if (stopping || finished)
    return
  stopping = true
  if (!child?.pid) {
    process.exitCode = 143
    if (process.connected)
      process.disconnect()
    return
  }

  const signal = (value: NodeJS.Signals) => {
    try {
      if (process.platform === 'win32')
        child!.kill(value)
      else
        process.kill(-child!.pid!, value)
    }
    catch { /* The process already exited. */ }
  }

  signal('SIGTERM')
  timer = setTimeout(signal, 2000, 'SIGKILL')
}

process.on('disconnect', stop)
process.on('SIGTERM', stop)
process.on('SIGINT', stop)
process.once('message', (message) => {
  if (message !== 'start' || stopping)
    return
  child = spawn(process.argv[2], process.argv.slice(3), { stdio: ['pipe', 'pipe', 'pipe'], detached: process.platform !== 'win32' })
  child.stdin?.on('error', () => {})
  process.stdin.pipe(child.stdin!)
  child.stdout?.pipe(process.stdout)
  child.stderr?.pipe(process.stderr)
  child.once('error', () => {
    process.stderr.write('Unable to start the run process.\n')
  })
  child.once('close', (code) => {
    finished = true
    clearTimeout(timer)
    process.exitCode = code ?? 143
    if (process.connected)
      process.disconnect()
  })
})
if (!process.connected)
  stop()
