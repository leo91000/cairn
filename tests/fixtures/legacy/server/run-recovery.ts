import type { Config } from './config.ts'
import type { Run } from './contracts.ts'
import type { prepareExecution } from './execution.ts'
import type { Store } from './store.ts'
import { readFile } from 'node:fs/promises'
import process from 'node:process'
import { codexSession } from './codex-rpc.ts'
import { AppError } from './errors.ts'
import { runnerSecret } from './execution.ts'

export interface ProcessIdentity { pid: number, start: string, boot: string }
export interface RunCheckpoint {
  prepared?: Awaited<ReturnType<typeof prepareExecution>>
  runnerId?: string
  generation?: string
  launched: boolean
  remainingMs: number | null
  process?: ProcessIdentity
  completed?: boolean
  lastMessage?: string
  settled?: Pick<Run, 'status' | 'summary' | 'finishedAt'>
}
export class RunRecovery {
  constructor(readonly store: Store, readonly config: Config) {}
  get(id: string) { return this.store.kv<RunCheckpoint>(`run-checkpoint:${id}`) }
  save(id: string, checkpoint: RunCheckpoint) { this.store.set(`run-checkpoint:${id}`, checkpoint) }
  async fence(run: Run) {
    const checkpoint = this.get(run.id)
    if (checkpoint?.process)
      await stopPreviousProcess(checkpoint.process)
    if (checkpoint?.prepared?.isolated || run.isolated) {
      if (!this.config.runnerUrl)
        throw new AppError(503, 'Waiting for the isolated runner before recovering this run.')
      const response = await fetch(`${this.config.runnerUrl}/runs/${checkpoint?.runnerId ?? run.id}`, { method: 'DELETE', headers: { authorization: `Bearer ${await runnerSecret(this.config.dataDir)}` }, signal: AbortSignal.timeout(10000) })
      await response.body?.cancel()
      if (!response.ok && response.status !== 404)
        throw new AppError(503, 'Waiting for the previous isolated container to stop.')
    }

    if (checkpoint) {
      delete checkpoint.process
      this.save(run.id, checkpoint)
    }
  }

  async session(run: Run, home: string, cwd: string) {
    try {
      return await codexSession(this.config)(home, async (rpc) => {
        if (run.sessionId) {
          const result = await rpc.request<{ thread: { id: string } }>('thread/read', { threadId: run.sessionId, includeTurns: false })
          if (result.thread?.id === run.sessionId)
            return run.sessionId
          throw new Error('Session mismatch')
        }

        const result = await rpc.request<{ data: { id: string, cwd: string, parentThreadId?: string | null }[] }>('thread/list', {
          limit: 2,
          cwd,
          sourceKinds: [run.chatExecution ? 'appServer' : 'exec'],
          archived: false,
        })
        const matches = result.data.filter(thread => thread.cwd === cwd && !thread.parentThreadId)
        if (matches.length === 1)
          return matches[0].id
        throw new Error('No unique session')
      })
    }
    catch { throw new AppError(409, 'The saved Codex conversation is unavailable. Working files were preserved; review them before starting a new run.') }
  }
}
export async function processIdentity(pid: number): Promise<ProcessIdentity | undefined> {
  if (process.platform !== 'linux')
    return undefined
  try {
    const stat = await readFile(`/proc/${pid}/stat`, 'utf8')
    const fields = stat.slice(stat.lastIndexOf(')') + 2).split(' ')
    if (fields[0] === 'Z')
      return undefined
    return { pid, start: fields[19], boot: (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim() }
  }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT')
      return undefined
    throw error
  }
}

async function stopPreviousProcess(identity: ProcessIdentity) {
  const matches = async () => {
    const current = await processIdentity(identity.pid)
    return current?.start === identity.start && current?.boot === identity.boot
  }

  if (!await matches())
    return
  try {
    process.kill(identity.pid, 'SIGTERM')
  }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'ESRCH')
      throw error
  }

  const deadline = Date.now() + 5000
  while (await matches()) {
    if (Date.now() >= deadline)
      throw new AppError(503, 'Waiting for the previous run process to stop.')
    await new Promise(resolve => setTimeout(resolve, 50))
  }
}
