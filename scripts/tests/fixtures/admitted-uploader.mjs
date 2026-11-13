import assert from 'node:assert/strict'
import childProcess from 'node:child_process'
import { syncBuiltinESMExports } from 'node:module'
import process from 'node:process'

// The exit fixtures qualify publication and foreground closure, not Node's
// runtime startup. Admit the synthetic uploader before its short upload clock;
// the real publisher still owns its output, expiry, cancellation and OS exit.
export function admittedUploader(publish, {
  uploader,
  env,
  deadline,
  onSpawn,
}) {
  return async (...args) => {
    const spawn = childProcess.spawn
    const child = spawn(process.execPath, [uploader], {
      env: { PATH: env.PATH, HOME: env.HOME },
      detached: true,
      stdio: ['pipe', 'pipe', 'pipe', 'pipe'],
    })
    child.stdin.on('error', () => {})
    let handedOff = false
    try {
      onSpawn(child)
      await new Promise((resolve, reject) => {
        let timer
        let text = ''

        function settle(error) {
          clearTimeout(timer)
          child.off('error', failed)
          child.off('exit', exited)
          child.stdio[3].off('data', ready)
          error ? reject(error) : resolve()
        }

        function failed(error) {
          settle(error)
        }

        function exited() {
          settle(new Error('Uploader fixture exited before admission'))
        }

        function ready(bytes) {
          text += bytes.toString()
          if (text === 'ready\n')
            settle()
          else if (!'ready\n'.startsWith(text))
            settle(new Error('Invalid uploader fixture admission'))
        }

        timer = setTimeout(() => settle(new Error('Uploader fixture admission exceeded the outer exit bound')), Math.max(0, deadline - Date.now()))
        child.once('error', failed)
        child.once('exit', exited)
        child.stdio[3].on('data', ready)
      })

      // artifactPublisher calls spawn synchronously before its first await.
      // Keep this substitution scoped to exactly its admitted uploader; other
      // commands and every subsequent publication still get distinct children.
      childProcess.spawn = (command, commandArgs, options) => {
        assert.equal(command, process.execPath)
        assert.deepEqual(commandArgs, [uploader])
        child.stdin.end(`${JSON.stringify(options.env)}\n`)
        handedOff = true
        return child
      }

      syncBuiltinESMExports()
      let result
      try {
        result = publish(...args)
      }
      finally {
        childProcess.spawn = spawn
        syncBuiltinESMExports()
      }

      return await result
    }
    finally {
      child.stdio[3].destroy()
      if (!handedOff) {
        child.kill('SIGKILL')
        child.stdin.destroy()
        child.stdout.destroy()
        child.stderr.destroy()
      }
    }
  }
}
