import type { ChatDetail } from '../../packages/contracts/chats'
import type {
  Agent,
  Project,
  Run,
  RunEvent,
} from '../../packages/contracts/contracts'
import { execFileSync } from 'node:child_process'
import path from 'node:path'
import process from 'node:process'

export interface FixtureConfig {
  dataDir: string
  home: string
  workspaceRoots: string[]
  publicUrl?: string
  host?: string
  port?: number
  codexBin?: string
  concurrency?: number
  logger?: boolean
  workerEnabled?: boolean
}

// This binary uses the current Rust Store, Vault and Service. Synthetic states such as
// interrupted runs and injected provider events deliberately have no public HTTP API.
export class NativeFixture {
  readonly config

  constructor(config: FixtureConfig) {
    this.config = {
      publicUrl: 'http://localhost:4310',
      host: '127.0.0.1',
      port: 0,
      codexBin: 'codex',
      concurrency: 1,
      logger: false,
      workerEnabled: false,
      runnerUrl: '',
      ...config,
    }
  }

  private call(operation: string, ...args: unknown[]): any {
    const output = execFileSync(process.env.CAIRN_BROWSER_FIXTURE_BINARY || path.resolve('target/debug/examples/browser_fixture'), [], {
      input: JSON.stringify({ config: this.config, operation, args }),
      encoding: 'utf8',
      maxBuffer: 64 * 1024 * 1024,
      stdio: ['pipe', 'pipe', 'pipe'],
    })
    return JSON.parse(output).result ?? undefined
  }

  /** Seeds the hook queue for engine/UI journeys; plugin_hooks tests cover transport validation. */
  emitHook = (instanceId: string, hook: string, payload: Record<string, unknown>, externalId: string) => this.call('emitHook', instanceId, hook, payload, externalId)

  // Test-created agents explicitly grant access to the shared development runner.
  agent = (input: Record<string, unknown>): Agent => this.call('agent', {
    access: { projects: null, skills: null, mcps: null },
    ...input,
  })

  project = (input: unknown): Project => this.call('project', input)
  queueChat = (input: { agentId?: string, projectId?: string }, title: string, text: string): ChatDetail => this.call('queueChat', input, title, text)

  /** An existing conversation owns the synthetic run used to seed timeline and artifacts. */
  chatRun = (chatId: string, title: string): Run => this.call('chatRun', chatId, title)
  skills = { save: (name: string, content: string) => this.call('skill', name, content) }
  chats = { detail: (id: string): ChatDetail => this.call('detail', id) }
  questions = { list: (id: string) => this.chats.detail(id).questions }
  vault = { set: (id: string, value: unknown) => this.call('vault', id, value) }
  store = {
    accountFixture: (id: string) => this.call('accountFixture', id),
    saveAccountFixture: (value: unknown) => this.call('saveAccountFixture', value),
    projects: (): Project[] => this.call('projects'),
    get: (kind: string, id: string) => this.call('get', kind, id),
    list: (kind: string): any[] => this.call('list', kind),
    put: (kind: string, value: unknown) => this.call('put', kind, value),
    run: (id: string) => this.call('run', id),
    runs: (): any[] => this.call('runs'),
    addRun: (value: unknown) => this.call('addRun', value),
    updateRun: (id: string, patch: unknown) => this.call('updateRun', id, patch),
    deferNativeCleanup: (id: string) => this.call('deferNativeCleanup', id),
    event: (id: string, type: string, text: string, payload?: unknown) => this.call('event', id, type, text, payload),
    seedEvents: (id: string, events: Pick<RunEvent, 'type' | 'text' | 'payload'>[]) => this.call('seedEvents', id, events),
    events: (id: string, after = 0, limit = 10000): RunEvent[] => this.call('events', id, after, limit),
    chatMessages: (id: string): any[] => this.call('chatMessages', id),
    insertArtifact: (value: unknown) => this.call('insertArtifact', value),
    set: (key: string, value: unknown, expires?: number) => this.call('set', key, value, expires),
    delete: (key: string) => this.call('delete', key),
    clearEvents: (id: string) => this.call('clearEvents', id),
  }
}
