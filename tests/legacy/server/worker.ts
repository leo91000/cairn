import type { ChildProcess } from 'node:child_process'
import type { ChatPlan } from './chat-process.ts'
import type { AccountLease } from './codex-accounts.ts'
import type { Run } from './contracts.ts'
import type { Service } from './service.ts'
import { Buffer } from 'node:buffer'
import { execFile, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { constants } from 'node:fs'
import {
  mkdir,
  open,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'
import { AppError, requireValue } from './errors.ts'
import {
  prepareCodexHome,
  prepareExecution,
  restoreExecution,
  runnerSecret,
} from './execution.ts'
import { maintenanceActive } from './maintenance.ts'
import { policy, runProjects } from './policy.ts'
import { processIdentity, RunRecovery } from './run-recovery.ts'
import { toolkitEnvironment } from './toolkit.ts'

const exec = promisify(execFile)

async function readSummary(file: string) {
  const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW).catch(() => undefined)
  if (!handle)
    return ''
  try {
    const buffer = Buffer.alloc(100000)
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0)
    return buffer.subarray(0, bytesRead).toString('utf8')
  }
  finally {
    await handle.close()
  }
}

export function redact(text: string) {
  return text
    .replace(
      /\b(?:gh[pousr]_\w{15,}|github_pat_\w{15,}|sk-[\w-]{12,})\b/g,
      '[redacted]',
    )
    .replace(/(Bearer\s+)[\w.~-]+/gi, '$1[redacted]')
    .replace(
      /("?(?:access_token|refresh_token|id_token|OPENAI_API_KEY|CODEX_API_KEY)"?\s*[:=]\s*"?)[^"\s,}]+/gi,
      '$1[redacted]',
    )
}

export function redactPayload(value: unknown, secrets: string[] = []): unknown {
  if (typeof value === 'string')
    return secrets.reduce((text, secret) => text.replaceAll(secret, '[redacted]'), redact(value))
  if (Array.isArray(value))
    return value.map(item => redactPayload(item, secrets))
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [
      key,
      /^(?:access_token|refresh_token|id_token|OPENAI_API_KEY|CODEX_API_KEY)$/i.test(key) ? '[redacted]' : redactPayload(item, secrets),
    ]))
  }

  return value
}

// Only Codex's terminal error events qualify, never assistant/tool output or generic HTTP 429s.
export function usageExhausted(event: Record<string, any>) {
  if (!['turn.failed', 'error'].includes(event.type))
    return false
  const error = event.type === 'turn.failed' ? event.error : event
  return error?.code === 'usage_limit_reached' || error?.codexErrorInfo === 'usageLimitExceeded'
    || (typeof error?.message === 'string' && /^(?:you['’]ve hit your usage limit|you have hit your usage limit|usage limit (?:has been )?(?:reached|exceeded))\b/i.test(error.message))
}

export function codexArgs(run: Run, output: string, sessionId?: string) {
  const a = run.snapshot.agent
  return [
    ...(policy(a).sandbox === 'yolo' ? ['--dangerously-bypass-approvals-and-sandbox'] : ['--sandbox', policy(a).sandbox, '-a', 'never']),
    '-c',
    'forced_login_method="chatgpt"',
    '-c',
    'cli_auth_credentials_store="file"',
    ...(sessionId && policy(a).sandbox === 'workspace-write' ? [path.dirname(output), ...run.workspaces?.map(workspace => workspace.path) ?? []].flatMap(directory => ['--add-dir', directory]) : []),
    'exec',
    ...(sessionId ? ['resume', sessionId] : []),
    '--json',
    '--skip-git-repo-check',
    ...(!sessionId ? ['--color', 'never'] : []),
    '-c',
    `model_reasoning_effort=${JSON.stringify(a.reasoning)}`,
    ...(a.model ? ['--model', a.model] : []),
    ...(policy(a).sandbox === 'workspace-write' ? ['-c', 'sandbox_workspace_write.network_access=true'] : []),
    ...(!sessionId && policy(a).sandbox === 'workspace-write' ? [path.dirname(output), ...run.workspaces?.map(workspace => workspace.path) ?? []].flatMap(directory => ['--add-dir', directory]) : []),
    '--output-last-message',
    output,
    '-',
  ]
}

export function runPrompt(run: Run, chat = false) {
  const interaction = chat
    ? 'This is an interactive chat. Use native user-input questions when clarification is useful. Nonblocking questions let you continue independent work while the user considers the options; a suggested answer is never user approval. Follow the latest user instructions and do not treat a question as authorization to publish changes.'
    : 'This unattended task cannot answer clarification questions; report a concrete blocker if required information is missing.'
  const projects = runProjects(run).map(project => `- ${project.name}: ${run.workspaces?.find(workspace => workspace.projectId === project.id)?.path ?? project.path}`).join('\n')
  return `${run.snapshot.agent.instructions}\n\n${run.snapshot.task.prompt}\n\nAvailable project workspaces (choose the relevant projects for this task):\n${projects || 'No projects assigned; use the task workspace.'}\n\nTooling: mise manages project runtimes and global tools. Prefer rg and fd for search. Respect mise.toml, .tool-versions, .nvmrc, .node-version, .python-version, rust-toolchain.toml, and package.json packageManager pins. Use mise exec -- <command> when project environment variables are needed; use uv for Python environments. Do not upgrade project pins unless the task requests it.\n\nSelected skills (use their supporting resources from the supplied paths):\n${run.snapshot.skills.map(s => `\n${s.path}\n${s.content}`).join('\n')}\n\nRun this task to completion within its stated scope. Preserve unrelated files. Do not expose credentials. ${interaction} All task-authorized effects such as creating PRs or releasing must follow their checks. Use .agents/skills for skills. Summarize actual changes, validation, external links and remaining blockers at the end.`
}

export class Worker {
  executions = new Set<Promise<void>>()
  active = new Map<
    string,
    {
      child: ChildProcess | null
      cancelled: boolean
      timedOut: boolean
      stopping: boolean
    }
  >()

  timer: NodeJS.Timeout | undefined
  busy = false
  closing = false
  lastMaintenance = 0
  readonly recovery: RunRecovery
  private initialized = false
  constructor(public service: Service) { this.recovery = new RunRecovery(service.store, service.config) }
  initialize() {
    if (this.initialized)
      return
    this.initialized = true
    for (const run of this.service.store.active()) {
      if (run.status !== 'running') {
        if (run.cancelRequestedAt)
          this.service.store.updateRun(run.id, { recoveryPending: true })
        continue
      }

      const checkpoint = this.recovery.get(run.id)
      if (!checkpoint && run.startedAt) {
        this.service.store.updateRun(run.id, { status: 'interrupted', finishedAt: Date.now(), summary: 'This older run has no restart checkpoint. Review its working files before retrying.' })
        continue
      }

      this.service.store.updateRun(run.id, {
        status: 'queued',
        recoveryPending: true,
        finishedAt: null,
        accountWaitReason: 'Recovering after worker restart.',
      })
      this.service.store.event(run.id, 'status', 'Recovering after worker restart')
    }
  }

  start() {
    this.initialize()
    this.timer = setInterval(() => void this.tick(), 1000)
    this.timer.unref()
    void this.tick()
  }

  async tick() {
    if (this.busy || this.closing)
      return
    this.busy = true
    this.initialize()
    try {
      if (Date.now() - this.lastMaintenance > 3600000) {
        this.service.store.maintain()
        this.lastMaintenance = Date.now()
      }

      await this.service.chats.tick(this)
      await this.service.schedule()
      if (this.closing || maintenanceActive(this.service.store))
        return
      const projects = new Set(
        [...this.active.keys()].map(
          id => runProjects(this.service.store.run(id)!).map(project => project.id),
        ).flat(),
      )
      for (const run of this.service.store.active()) {
        if (this.active.size >= this.service.config.concurrency)
          break
        if (run.status !== 'queued' || runProjects(run).some(project => projects.has(project.id)))
          continue
        // Saved conversations retain their project lock while waiting for recovery or an account.
        if (run.recoveryPending || this.recovery.get(run.id)?.launched) {
          for (const project of runProjects(run))
            projects.add(project.id)
        }

        if (run.recoveryPending) {
          try {
            await this.recovery.fence(run)
            this.service.mcps.revokeRun(run.id)
            await this.service.accounts.recoverRun(run)
            if (this.service.store.run(run.id)?.cancelRequestedAt) {
              this.service.store.updateRun(run.id, {
                status: 'cancelled',
                finishedAt: Date.now(),
                accountWaitReason: null,
                recoveryPending: false,
              })
              continue
            }

            const checkpoint = this.recovery.get(run.id)
            if (checkpoint?.settled) {
              this.service.store.updateRun(run.id, { ...checkpoint.settled, accountWaitReason: null, recoveryPending: false })
              continue
            }

            if (checkpoint?.completed) {
              this.service.store.updateRun(run.id, {
                status: 'succeeded',
                finishedAt: Date.now(),
                resumeAvailable: false,
                accountWaitReason: null,
                recoveryPending: false,
                summary: checkpoint.lastMessage || 'Conversation completed before worker restart. See Activity for the recorded result.',
              })
              continue
            }

            this.service.store.updateRun(run.id, { recoveryPending: false })
          }
          catch {
            const message = 'Waiting for the previous execution to stop before recovery.'
            if (run.accountWaitReason !== message)
              this.service.store.updateRun(run.id, { accountWaitReason: message })
            continue
          }
        }

        let account: AccountLease | null
        try {
          account = await this.service.accounts.acquire(run.id, run.snapshot.agent.model)
        }
        catch (error) {
          const message = error instanceof AppError ? error.message : 'Waiting for Codex account availability.'
          if (run.accountWaitReason !== message) {
            this.service.store.updateRun(run.id, { accountWaitReason: message })
            this.service.store.event(run.id, 'status', message)
          }

          continue
        }

        if (this.closing || this.service.store.run(run.id)?.status !== 'queued') {
          if (account)
            await this.service.accounts.release(account)
          continue
        }

        for (const project of runProjects(run))
          projects.add(project.id)
        this.active.set(run.id, {
          child: null,
          cancelled: false,
          timedOut: false,
          stopping: false,
        })
        const execution = this.execute(run, account)
        this.executions.add(execution)
        void execution.finally(() => this.executions.delete(execution))
      }
    }
    catch (e) {
      this.service.store.audit('worker.error', {
        message: (e as Error).message,
      })
    }
    finally {
      this.busy = false
    }
  }

  async execute(run: Run, account: AccountLease | null = null) {
    const { store, config } = this.service
    const control = this.active.get(run.id)!
    let timeout: NodeJS.Timeout | undefined
    let heartbeat: NodeJS.Timeout | undefined
    let sensitive: string[] = []
    const saved = this.recovery.get(run.id)
    const checkpoint = saved ?? { launched: false, remainingMs: (run.snapshot.agent.timeoutMinutes > 0 ? run.snapshot.agent.timeoutMinutes * 60000 : null) }
    const deadline = checkpoint.remainingMs === null ? null : Date.now() + checkpoint.remainingMs
    const timeExpired = () => deadline !== null && Date.now() >= deadline
    const checkpointBudget = () => {
      checkpoint.remainingMs = deadline === null ? null : Math.max(0, deadline - Date.now())
      this.recovery.save(run.id, checkpoint)
    }

    const sanitize = (text: string) => sensitive.reduce((value, secret) => value.replaceAll(secret, '[redacted]'), redact(text))
    try {
      checkpointBudget()
      store.updateRun(run.id, {
        status: 'running',
        startedAt: run.startedAt ?? Date.now(),
        finishedAt: null,
        accountWaitReason: null,
        codexAccountId: account?.accountId ?? null,
        codexAccountName: account ? this.service.accounts.get(account.accountId).name : null,
      })
      if (account)
        store.event(run.id, 'status', `Using Codex account: ${this.service.accounts.get(account.accountId).name}`)
      store.event(run.id, 'status', 'Preparing workspace')
      const directory = path.join(config.dataDir, 'runs', run.id)
      await mkdir(directory, { recursive: true, mode: 0o700 })
      const currentAgent = requireValue(store.get('agents', run.snapshot.agent.id))
      if (JSON.stringify(policy(currentAgent)) !== JSON.stringify(policy(run.snapshot.agent)))
        throw new AppError(409, 'Agent access changed after this run was queued. Run the task again with the current policy.')
      if (saved && !saved.prepared)
        checkpoint.generation = randomUUID().slice(0, 8)
      checkpointBudget()
      heartbeat = setInterval(checkpointBudget, 5000)
      heartbeat.unref()
      const prepared = checkpoint.prepared
        ? await restoreExecution(run, checkpoint.prepared, config, store.kv<string>(`agent-github:${run.snapshot.agent.id}`))
        : await prepareExecution(run, config, store.kv<string>(`agent-github:${run.snapshot.agent.id}`), account?.home, checkpoint.generation)
      checkpoint.prepared = prepared
      checkpointBudget()
      const codexHome = prepared.isolated ? path.join(directory, 'home', '.codex') : path.join(directory, 'codex')
      await mkdir(codexHome, { recursive: true, mode: 0o700 })
      if (!prepared.isolated)
        await prepareCodexHome(config, codexHome)
      if (account && prepared.isolated)
        await this.service.accounts.relocate(account, path.join(directory, 'home', '.codex'))
      Object.assign(run, { workspace: prepared.cwd, workspaces: prepared.workspaces, isolated: prepared.isolated })
      store.updateRun(run.id, { workspace: run.workspace, workspaces: run.workspaces, isolated: run.isolated })
      if (control.cancelled || control.stopping)
        throw new Error('Execution stopped before launch')
      const output = prepared.output
      const env = prepared.isolated ? { ...process.env } : await toolkitEnvironment(config.home)
      env.HOME = config.home
      env.CODEX_HOME = codexHome
      delete env.OPENAI_API_KEY
      delete env.CODEX_API_KEY
      delete env.CODEX_THREAD_ID
      const mcp = this.service.mcps.runConfiguration(run)
      sensitive = [...mcp.redactions, ...account ? this.service.accounts.redactions(account.accountId) : []]
      Object.assign(env, mcp.env)
      let total = 0
      const max = 5_000_000
      let resumeSession = checkpoint.launched ? await this.recovery.session(run, codexHome, prepared.cwd) : undefined
      if (resumeSession) {
        store.updateRun(run.id, { sessionId: resumeSession, resumeCount: (run.resumeCount ?? 0) + 1, resumeAvailable: true })
        store.event(run.id, 'status', 'Resuming saved conversation and workspace')
      }

      while (true) {
        if (control.cancelled || control.stopping || timeExpired()) {
          control.timedOut = !control.cancelled && !control.stopping
          throw new Error(control.cancelled ? 'Run cancelled.' : 'Run exceeded its time limit.')
        }

        await rm(output, { force: true })
        env.CODEX_HOME = account?.home ?? codexHome
        let prompt = resumeSession ? 'Continue the same task from the saved conversation and current workspace. Execution was interrupted. Resume the original task from its last completed step. Preserve completed work and verify external effects before repeating any action.' : runPrompt(run)
        let binary = config.codexBin
        let args = [...mcp.args, ...codexArgs(run, output, resumeSession)]
        let chat: ChatPlan | undefined
        if (run.chatExecution) {
          const inputDirectory = path.join(directory, 'chat-input')
          await mkdir(inputDirectory, { recursive: true, mode: 0o700 })
          chat = {
            execution: run.chatExecution,
            sessionId: resumeSession,
            instructions: runPrompt({ ...run, snapshot: { ...run.snapshot, task: { ...run.snapshot.task, prompt: '' }, skills: prepared.isolated ? prepared.skills : run.snapshot.skills } }, true),
            inputDirectory: prepared.isolated ? '/run/leo-chat' : inputDirectory,
            output,
            cwd: prepared.cwd,
            model: run.snapshot.agent.model,
            reasoning: run.snapshot.agent.reasoning,
            sandbox: policy(run.snapshot.agent).sandbox,
            writableRoots: [path.dirname(output), ...prepared.workspaces.map(workspace => workspace.path)],
            args: mcp.args,
          }
          binary = process.execPath
          args = ['--import', import.meta.resolve('tsx'), path.join(import.meta.dirname, 'chat-process.ts'), config.codexBin]
          prompt = JSON.stringify(chat)
        }

        if (prepared.isolated) {
          checkpoint.runnerId = randomUUID()
          checkpointBudget()
          const plans = path.join(config.dataDir, 'runner-plans')
          await mkdir(plans, { recursive: true, mode: 0o700 })
          await writeFile(path.join(plans, `${checkpoint.runnerId}.json`), JSON.stringify({
            id: checkpoint.runnerId,
            ...(chat ? { chat } : {}),
            args,
            cwd: prepared.cwd,
            prompt: resumeSession ? prompt : runPrompt({ ...run, snapshot: { ...run.snapshot, skills: prepared.skills } }),
            mounts: [...prepared.mounts, ...(chat ? [{ source: path.join(directory, 'chat-input'), target: '/run/leo-chat', readOnly: true }] : [])],
            expires: deadline,
            sandbox: policy(run.snapshot.agent).sandbox,
            mcpEnv: mcp.env,
          }), { mode: 0o600 })
          env.RUNNER_URL = config.runnerUrl
          env.RUNNER_TOKEN = await runnerSecret(config.dataDir)
          binary = process.execPath
          args = ['--import', import.meta.resolve('tsx'), path.join(import.meta.dirname, 'runner-client.ts'), checkpoint.runnerId]
        }

        const child = spawn(process.execPath, ['--import', import.meta.resolve('tsx'), path.join(import.meta.dirname, 'run-supervisor.ts'), binary, ...args], {
          cwd: prepared.cwd,
          env,
          stdio: ['pipe', 'pipe', 'pipe', 'ipc'],
          detached: process.platform !== 'win32',
        })
        control.child = child
        const completion = new Promise<number | null>((resolve, reject) => {
          child.once('error', reject)
          child.once('close', resolve)
        })
        void completion.catch(() => {})
        // Record ownership before allowing the supervisor to launch Codex.
        checkpoint.process = child.pid ? await processIdentity(child.pid) : undefined
        if (run.chatExecution) {
          run.chatExecution = { ...run.chatExecution, recovery: true }
          store.updateRun(run.id, { chatExecution: run.chatExecution })
        }

        checkpoint.launched = true
        checkpoint.completed = false
        checkpointBudget()
        child.on('error', () => {})
        if (!control.stopping && !control.cancelled)
          child.send('start', () => {})
        else
          this.kill(child)
        if (deadline !== null) {
          timeout = setTimeout(() => {
            control.timedOut = true
            this.kill(child)
          }, Math.max(1, deadline - Date.now()))
        }

        let exhausted = false
        let buffer = ''
        const line = (raw: string) => {
          try {
            const event = JSON.parse(raw)
            if (event.type === 'chat.question') {
              this.service.questions.receive(run.id, event.question)
              return
            }

            if (event.type === 'chat.question.closed') {
              this.service.questions.release(run.id, event.questionId)
              return
            }

            if (event.type === 'chat.delivered' && typeof event.messageId === 'string') {
              this.service.chats.acknowledge(run.id, event.messageId)
              return
            }

            if (usageExhausted(event))
              exhausted = true
            if (
              event.type === 'thread.started'
              && typeof event.thread_id === 'string'
            ) {
              store.updateRun(run.id, { sessionId: event.thread_id, resumeAvailable: true })
            }

            if (event.item?.type === 'agent_message' && typeof event.item.text === 'string')
              checkpoint.lastMessage = sanitize(event.item.text).slice(0, 100000)
            if (event.type === 'turn.completed') {
              checkpoint.completed = true
              checkpointBudget()
              if (event.usage)
                store.updateRun(run.id, { usage: event.usage })
            }

            if (total >= max)
              return
            total += raw.length
            const text
              = event.item?.text
                ?? event.item?.aggregated_output
                ?? event.error?.message
                ?? raw
            store.event(
              run.id,
              event.type ?? 'output',
              sanitize(typeof text === 'string' ? text : JSON.stringify(text)),
              redactPayload(event, sensitive) as Record<string, unknown>,
            )
          }
          catch {
            if (total >= max)
              return
            total += raw.length
            store.event(run.id, 'output', sanitize(raw))
          }
        }

        child.stdout?.on('data', (chunk) => {
          buffer += chunk.toString()
          let split = buffer.indexOf('\n')
          while (split >= 0) {
            line(buffer.slice(0, Math.min(split, 64000)))
            buffer = buffer.slice(split + 1)
            split = buffer.indexOf('\n')
          }

          if (buffer.length > 64000) {
            line(buffer.slice(0, 64000))
            buffer = ''
          }
        })
        child.stderr?.on('data', (chunk) => {
          if (total >= max)
            return
          total += chunk.length
          store.event(run.id, 'diagnostic', sanitize(chunk.toString()))
        })
        child.stdin?.on('error', () => {})
        child.stdin?.end(prompt)
        const code = await completion
        if (buffer)
          line(buffer)
        clearTimeout(timeout)
        control.child = null
        delete checkpoint.process
        if (checkpoint.runnerId)
          await rm(path.join(config.dataDir, 'runner-plans', `${checkpoint.runnerId}.json`), { force: true }).catch(() => {})
        checkpointBudget()
        if (prepared.isolated)
          await this.recovery.fence(store.run(run.id)!)
        if (control.stopping && !control.cancelled && !checkpoint.completed)
          throw new Error('Worker is restarting')
        const sessionId = store.run(run.id)?.sessionId
        if (exhausted && code !== 0 && account && !control.cancelled && !control.stopping && !control.timedOut && sessionId) {
          const previous = account
          this.service.accounts.markExhausted(previous.accountId, run.snapshot.agent.model)
          await this.service.accounts.release(previous)
          account = null
          store.updateRun(run.id, { accountWaitReason: 'Usage exhausted. Waiting for an available Codex account to resume.', codexAccountId: null, codexAccountName: null })
          store.event(run.id, 'status', 'Usage exhausted. Saving this session and restoring capacity or switching accounts.')
          await this.service.accounts.refresh(previous.accountId)
          while (!account && !control.cancelled && !control.stopping && !timeExpired()) {
            try {
              account = await this.service.accounts.acquire(run.id, run.snapshot.agent.model)
            }
            catch (error) {
              if (!(error instanceof AppError && error.statusCode === 409))
                throw error
              await new Promise(resolve => setTimeout(resolve, 1000))
            }
          }

          if (account) {
            if (prepared.isolated)
              await this.service.accounts.relocate(account, previous.home)
            if (!prepared.isolated)
              await prepareCodexHome(config, account.home)
            sensitive.push(...this.service.accounts.redactions(account.accountId))
            const name = this.service.accounts.get(account.accountId).name
            store.updateRun(run.id, { accountWaitReason: null, codexAccountId: account.accountId, codexAccountName: name })
            store.event(run.id, 'status', `Resuming saved session with Codex account: ${name}`)
          }

          resumeSession = sessionId
          continue
        }

        const summary = redact(await readSummary(output) || checkpoint.lastMessage || '')
        const status = control.cancelled
          ? 'cancelled'
          : control.timedOut
            ? 'failed'
            : code === 0 || (control.stopping && checkpoint.completed)
              ? 'succeeded'
              : 'failed'
        store.updateRun(run.id, {
          status,
          finishedAt: Date.now(),
          resumeAvailable: (status !== 'succeeded' || !!run.chatExecution) && !!store.run(run.id)?.sessionId,
          summary:
          sanitize(summary).slice(0, 100000)
          || (control.cancelled
            ? 'Run stopped. You can resume this conversation.'
            : control.timedOut
              ? 'Run exceeded its time limit.'
              : `Process exited with code ${code}.`),
        })
        store.event(run.id, 'status', status)
        break
      }
    }
    catch (e) {
      const status = control.cancelled ? 'cancelled' : control.stopping ? 'queued' : 'failed'
      const needsFence = !!checkpoint.prepared?.isolated && !!checkpoint.runnerId
      const summary = sanitize((e as Error).message)
      if (needsFence && status !== 'queued') {
        checkpoint.settled = { status, summary, finishedAt: Date.now() }
        checkpointBudget()
      }

      store.updateRun(run.id, {
        status: needsFence ? 'queued' : status,
        recoveryPending: needsFence || status === 'queued',
        finishedAt: needsFence || status === 'queued' ? null : Date.now(),
        summary,
        accountWaitReason: needsFence ? 'Waiting for the previous isolated container to stop.' : status === 'queued' ? 'Paused for worker restart. This run will resume automatically.' : null,
      })
      store.event(run.id, control.stopping ? 'status' : 'error', sanitize((e as Error).message))
    }
    finally {
      clearInterval(heartbeat)
      if (control.child) {
        const stopped = new Promise<void>(resolve => control.child!.once('close', () => resolve()))
        this.kill(control.child)
        await stopped
        control.child = null
      }

      let fenced = true
      if (checkpoint.prepared?.isolated && checkpoint.runnerId) {
        try {
          await this.recovery.fence(store.run(run.id)!)
        }
        catch {
          fenced = false
          const current = store.run(run.id)!
          if (current.status !== 'queued')
            checkpoint.settled = { status: current.status, summary: current.summary, finishedAt: current.finishedAt }
          store.updateRun(run.id, {
            status: 'queued',
            recoveryPending: true,
            finishedAt: null,
            accountWaitReason: 'Waiting for the previous isolated container to stop.',
          })
        }
      }

      checkpointBudget()
      if (fenced && checkpoint.settled)
        store.updateRun(run.id, { ...checkpoint.settled, recoveryPending: false, accountWaitReason: null })
      if (account && fenced) {
        const releasedId = account.accountId
        await this.service.accounts.release(account).catch(() => {
          store.audit('codex.account.release_failed', { id: releasedId, runId: run.id })
        })
      }

      this.service.mcps.revokeRun(run.id)
      if (timeout)
        clearTimeout(timeout)
      if (fenced) {
        for (const home of ['codex', 'home/.codex'])
          await rm(path.join(config.dataDir, 'runs', run.id, home, 'auth.json'), { force: true }).catch(() => {})
        await rm(path.join(config.dataDir, 'runs', run.id, 'home', '.config', 'gh'), { recursive: true, force: true }).catch(() => {})
      }

      // Session history, isolated skills and working files survive shutdown and cancellation.
      await rm(path.join(config.dataDir, 'runner-plans', `${checkpoint.runnerId ?? run.id}.json`), { force: true }).catch(() => {})
      this.active.delete(run.id)
    }
  }

  kill(child: ChildProcess) {
    if (!child.pid)
      return
    const signal = (value: NodeJS.Signals) => {
      try {
        if (process.platform === 'win32')
          child.kill(value)
        else
          process.kill(-child.pid!, value)
      }
      catch {
        /* already stopped */
      }
    }

    signal('SIGTERM')
    const timer = setTimeout(signal, 3000, 'SIGKILL')
    timer.unref()
    child.once('close', () => clearTimeout(timer))
  }

  cancel(id: string) {
    const run = requireValue(this.service.store.run(id))
    if (!['queued', 'running'].includes(run.status))
      throw new AppError(409, 'This run has already finished.')
    this.service.store.updateRun(id, { cancelRequestedAt: Date.now() })
    const active = this.active.get(id)
    if (active) {
      active.cancelled = true
      if (active.child)
        this.kill(active.child)
    }
    else if (!run.recoveryPending) {
      this.service.store.updateRun(id, {
        status: 'cancelled',
        finishedAt: Date.now(),
        summary: 'Cancelled before execution.',
      })
    }

    this.service.store.audit('run.cancelled', { id })
  }

  resume(id: string) {
    const run = requireValue(this.service.store.run(id))
    const checkpoint = this.recovery.get(id)
    if (this.active.has(id))
      throw new AppError(409, 'Wait for this run to finish stopping before resuming.')
    if (run.chatExecution && ['failed', 'cancelled'].includes(run.status) && !checkpoint?.launched) {
      if (checkpoint) {
        checkpoint.remainingMs = (run.snapshot.agent.timeoutMinutes > 0 ? run.snapshot.agent.timeoutMinutes * 60000 : null)
        delete checkpoint.settled
        this.recovery.save(id, checkpoint)
      }

      return this.service.store.updateRun(id, {
        status: 'queued',
        recoveryPending: true,
        cancelRequestedAt: null,
        finishedAt: null,
        accountWaitReason: null,
      })
    }

    if (!['failed', 'interrupted', 'cancelled'].includes(run.status) || !run.resumeAvailable || !checkpoint?.prepared || run.workspaceCleanedAt)
      throw new AppError(409, 'This run has no saved conversation available to resume.')
    if (this.service.store.active().some(active => active.taskId === run.taskId))
      throw new AppError(409, 'This task already has an active run.')
    checkpoint.remainingMs = (run.snapshot.agent.timeoutMinutes > 0 ? run.snapshot.agent.timeoutMinutes * 60000 : null)
    checkpoint.completed = false
    delete checkpoint.settled
    this.recovery.save(id, checkpoint)
    this.service.store.updateRun(id, {
      status: 'queued',
      recoveryPending: true,
      cancelRequestedAt: null,
      finishedAt: null,
      accountWaitReason: 'Resuming saved conversation.',
    })
    this.service.store.event(id, 'status', 'Resume requested')
    return this.service.store.run(id)!
  }

  async cleanup(id: string) {
    const run = requireValue(this.service.store.run(id))
    if (['queued', 'running'].includes(run.status)) {
      throw new AppError(
        409,
        'Wait for this run to finish before cleaning up.',
      )
    }

    if (run.isolated)
      throw new AppError(409, 'Isolated clones are retained for review. After preserving your work, remove their directory through the server terminal.')
    const workspaces = run.workspaces ?? (run.workspace && run.snapshot.project ? [{ projectId: run.snapshot.project.id, path: run.workspace, kind: 'worktree' as const }] : [])
    const managed = workspaces.filter(workspace => workspace.kind !== 'direct')
    if (!run.snapshot.task.worktree || !run.workspace || !managed.length)
      throw new AppError(409, 'This run has no managed worktree to clean up.')
    const generation = this.recovery.get(id)?.generation
    const root = path.join(this.service.config.dataDir, 'runs', id, generation ? `workspace-${generation}` : 'workspace')
    for (const workspace of managed) {
      if (workspace.path !== root && !workspace.path.startsWith(root + path.sep))
        throw new AppError(409, 'Workspace is outside this run’s managed directory.')
      const status = await exec('git', ['-C', workspace.path, 'status', '--porcelain', '--ignored'], { timeout: 10000, maxBuffer: 100000 })
      if (status.stdout.trim())
        throw new AppError(409, 'This worktree contains changes or untracked files. Commit or move them before cleanup.')
    }

    for (const workspace of managed) {
      if (workspace.kind === 'clone') {
        await rm(workspace.path, { recursive: true })
        continue
      }

      const project = requireValue(runProjects(run).find(project => project.id === workspace.projectId))
      await exec('git', ['-C', project.path, 'worktree', 'remove', workspace.path], { timeout: 10000, maxBuffer: 100000 })
    }

    this.service.store.updateRun(id, {
      workspace: null,
      resumeAvailable: false,
      workspaceCleanedAt: Date.now(),
    })
    this.service.store.audit('run.workspace.cleaned', { id })
    return { cleaned: true }
  }

  async close() {
    this.closing = true
    if (this.timer)
      clearInterval(this.timer)
    for (const value of this.active.values()) {
      value.stopping = true
      if (value.child)
        this.kill(value.child)
    }

    while (this.busy)
      await new Promise(r => setTimeout(r, 30))
    await Promise.allSettled(this.executions)
  }
}
