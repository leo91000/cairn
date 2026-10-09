import type { Config } from './config.ts'
import type {
  Agent,
  Project,
  Run,
  Task,
} from './contracts.ts'
import type { Store } from './store.ts'
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { promisify } from 'node:util'
import { CronExpressionParser } from 'cron-parser'
import {
  agentInput,
  MAIN_AGENT_ID,
  projectInput,
  taskInput,
} from '../../../../packages/contracts/contracts.ts'
import { ChatQuestions } from './chat-questions.ts'
import { Chats } from './chats.ts'
import { CodexAccounts } from './codex-accounts.ts'
import { AppError, requireValue } from './errors.ts'
import { McpConnections } from './mcp-connections.ts'
import { Notifications } from './notifications.ts'
import { workspaceDirectory } from './paths.ts'
import {
  allowedProjects,
  policy,
  runProjects,
  taskProjects,
  validateAccess,
} from './policy.ts'
import { Skills } from './skills.ts'

const exec = promisify(execFile)

async function projectOrigin(directory: string) {
  try {
    const { stdout } = await exec('git', ['-C', directory, 'config', '--get', 'remote.origin.url'], { timeout: 3000, maxBuffer: 8000 })
    const remote = stdout.trim()
    const scp = remote.match(/^(?:[\w.-]+@)?([a-z0-9.-]+):([\w./-]+)$/i)
    if (scp)
      return `${scp[1]}:${scp[2]}`
    const url = new URL(remote)
    if (!['https:', 'http:', 'ssh:', 'git:'].includes(url.protocol))
      return ''
    return `${url.protocol}//${url.host}${url.pathname}`
  }
  catch {
    return ''
  }
}

export function nextOccurrences(
  cron: string,
  timezone: string,
  now = Date.now(),
  count = 3,
) {
  try {
    if (cron.trim().split(/\s+/).length !== 5)
      throw new Error('Cron requires five fields')
    new Intl.DateTimeFormat('en', { timeZone: timezone }).resolvedOptions()
    const expression = CronExpressionParser.parse(cron, {
      tz: timezone,
      currentDate: new Date(now),
    })
    return Array.from({ length: count }, () => expression.next().getTime())
  }
  catch {
    throw new AppError(
      400,
      'Enter a valid five-field cron expression and IANA timezone.',
    )
  }
}

export class Service {
  readonly chats = new Chats(this)
  readonly questions = new ChatQuestions(this)
  notifications: Notifications
  skills: Skills
  mcps: McpConnections
  accounts: CodexAccounts
  constructor(
    public store: Store,
    public config: Config,
  ) {
    this.notifications = new Notifications(store, config)
    this.mcps = new McpConnections(store, config)
    this.accounts = new CodexAccounts(store, config)
    this.skills = new Skills(config.home, config.workspaceRoots)
    this.store.transaction(() => {
      for (const agent of store.list('agents')) {
        if (!agent.access || !Object.hasOwn(agent.access, 'mcps'))
          store.put('agents', { ...agent, access: policy(agent) })
      }

      if (!store.get('agents', MAIN_AGENT_ID))
        store.put('agents', { ...agentInput.parse({ name: 'Main agent', description: 'Your default agent, with access to every registered project, skill, and shared connection.' }), id: MAIN_AGENT_ID, createdAt: Date.now() })
    })
  }

  agent(value: unknown, existingId?: string) {
    const input = agentInput.parse(value)
    const existing = existingId ? requireValue(this.store.get('agents', existingId)) : undefined
    if (existing && value && typeof value === 'object' && !Object.hasOwn(value, 'access'))
      input.access = policy(existing)
    const item: Agent = {
      ...input,
      id: existingId ?? randomUUID(),
      createdAt:
        this.store.get('agents', existingId ?? '')?.createdAt ?? Date.now(),
    }
    validateAccess(item)
    for (const projectId of item.access.projects ?? [])
      requireValue(this.store.get('projects', projectId), 'Allowed project not found')
    if (existingId === MAIN_AGENT_ID && (item.access.projects !== null || item.access.skills !== null || !item.access.github || item.access.mcps !== null || Object.keys(item.access.mcpTools).length > 0))
      throw new AppError(400, 'The main agent always has access to all resources. Create another agent for restricted access.')
    for (const id of item.access.mcps ?? [])
      this.mcps.get(id)
    for (const id of Object.keys(item.access.mcpTools)) {
      this.mcps.get(id)
      if (item.access.mcps !== null && !item.access.mcps.includes(id))
        throw new AppError(400, 'Tool permissions require access to the MCP connection.')
    }

    this.store.put('agents', item)
    this.store.audit('agent.saved', { id: item.id })
    return item
  }

  async project(value: unknown, existingId?: string) {
    const input = projectInput.parse(value)
    const actual = await workspaceDirectory(
      input.path,
      this.config.workspaceRoots,
    )
    if (existingId)
      requireValue(this.store.get('projects', existingId))
    const item: Project = {
      ...input,
      path: actual,
      origin: await projectOrigin(actual),
      id: existingId ?? randomUUID(),
      createdAt:
        this.store.get('projects', existingId ?? '')?.createdAt ?? Date.now(),
    }
    this.store.put('projects', item)
    this.store.audit('project.saved', { id: item.id })
    return item
  }

  task(value: unknown, existingId?: string) {
    const input = taskInput.parse(value)
    if (input.cron)
      nextOccurrences(input.cron, input.timezone)
    const agent = requireValue(this.store.get('agents', input.agentId), 'Agent not found')
    taskProjects(agent, input as Task, this.store.list('projects'))
    if (input.skills && policy(agent).skills !== null && input.skills.some(key => !policy(agent).skills!.includes(key)))
      throw new AppError(400, 'A task cannot use skills outside its agent’s access.')
    if (existingId)
      requireValue(this.store.get('tasks', existingId))
    const item: Task = {
      ...input,
      id: existingId ?? randomUUID(),
      createdAt:
        this.store.get('tasks', existingId ?? '')?.createdAt ?? Date.now(),
      nextRun:
        input.cron && input.enabled && !input.archived
          ? nextOccurrences(input.cron, input.timezone)[0]
          : null,
    }
    this.store.put('tasks', item)
    this.store.audit('task.saved', { id: item.id })
    return item
  }

  remove(kind: 'agents' | 'projects' | 'tasks', id: string) {
    requireValue(this.store.get(kind, id))
    if (kind === 'agents' && id === MAIN_AGENT_ID)
      throw new AppError(409, 'The main agent cannot be removed.')
    if (kind === 'projects' && this.store.list('agents').some(agent => policy(agent).projects?.includes(id)))
      throw new AppError(409, 'This project is assigned to an agent. Update the agent first.')
    if (
      kind !== 'tasks'
      && this.store
        .list('tasks')
        .some(t =>
          kind === 'agents' ? t.agentId === id : t.projectId === id,
        )
    ) {
      throw new AppError(
        409,
        'This item is used by a task. Update or remove that task first.',
      )
    }

    if (
      this.store
        .active()
        .some(r =>
          kind === 'tasks'
            ? r.taskId === id
            : kind === 'projects'
              ? runProjects(r).some(project => project.id === id)
              : r.snapshot.agent.id === id,
        )
    ) {
      throw new AppError(
        409,
        'This item has active work. Cancel or wait for the run first.',
      )
    }

    if (kind === 'agents')
      this.store.delete(`agent-github:${id}`)
    this.store.remove(kind, id)
    this.store.audit(`${kind}.deleted`, { id })
  }

  async agentSkills(agent: Agent) {
    const access = policy(agent)
    const available = await this.skills.list()
    for (const project of allowedProjects(agent, this.store.list('projects')))
      available.push(...await this.skills.list(project.id, project.path))
    return available.filter(skill => access.skills === null || access.skills.includes(`${skill.scope}/${skill.name}`))
  }

  async enqueue(
    taskId: string,
    trigger = 'manual',
    dedupe: string | null = null,
  ): Promise<Run> {
    const task = requireValue(
      this.store.get('tasks', taskId),
      'Task not found',
    )
    const run = await this.snapshotRun(task, trigger)
    try {
      this.store.addRun(run, dedupe)
    }
    catch (e) {
      if ((e as Error).message.includes('UNIQUE constraint')) {
        throw new AppError(
          409,
          'This task already has active work or this occurrence was already queued.',
        )
      }

      throw e
    }

    this.store.event(run.id, 'status', 'Queued')
    this.store.audit('run.queued', { id: run.id, taskId, trigger })
    return run
  }

  async snapshotRun(task: Task, trigger: string): Promise<Run> {
    if (task.archived)
      throw new AppError(409, 'Restore this archived task before running it.')
    const agent = requireValue(this.store.get('agents', task.agentId))
    const projects = taskProjects(agent, task, this.store.list('projects'))
    const project = projects.length === 1 ? projects[0] : null
    const access = policy(agent)
    const available = (await this.agentSkills(agent)).filter(skill => skill.scope === 'global' || projects.some(project => project.id === skill.scope))
    const keys = task.skills ?? available.filter(skill => skill.valid).map(skill => `${skill.scope}/${skill.name}`)
    const skills = keys.map((key) => {
      if (access.skills !== null && !access.skills.includes(key))
        throw new AppError(400, `Skill outside agent access: ${key}`)
      const selected = available.find(s => `${s.scope}/${s.name}` === key)
      if (!selected?.valid)
        throw new AppError(400, `Skill unavailable or invalid: ${key}`)
      return { name: selected.name, path: selected.path, content: selected.content }
    })
    const run: Run = {
      id: randomUUID(),
      taskId: task.id,
      projectId: project?.id ?? null,
      status: 'queued',
      trigger,
      createdAt: Date.now(),
      startedAt: null,
      finishedAt: null,
      summary: '',
      sessionId: null,
      workspace: null,
      usage: null,
      snapshot: {
        task,
        agent,
        project,
        projects,
        skills,
      },
    }
    return run
  }

  async schedule(now = Date.now()) {
    for (const task of this.store.list('tasks')) {
      if (
        !task.enabled
        || task.archived
        || !task.cron
        || task.nextRun === null
        || task.nextRun > now
      ) {
        continue
      }

      try {
        await this.enqueue(task.id, 'schedule', `${task.id}:${task.nextRun}`)
      }
      catch (e) {
        if (!(e instanceof AppError && e.statusCode === 409)) {
          this.store.audit('schedule.failed', {
            taskId: task.id,
            error: (e as Error).message,
          })
        }
      }

      const current = this.store.get('tasks', task.id)
      if (current?.nextRun === task.nextRun) {
        this.store.put('tasks', {
          ...current,
          nextRun: nextOccurrences(task.cron, task.timezone, now)[0],
        })
      }
    }
  }
}
