import type { Chat, ChatMessage } from '../../../../packages/contracts/chats.ts'
import type { McpConnection } from '../../../../packages/contracts/mcp.ts'
import type { CodexAccount } from './codex-account-types.ts'
import type {
  Agent,
  Project,
  Run,
  RunEvent,
  RunListItem,
  RunStatus,
  Task,
} from './contracts.ts'
import { mkdirSync } from 'node:fs'
import path from 'node:path'
import { DatabaseSync } from 'node:sqlite'

interface Records {
  chats: Chat
  codexAccounts: CodexAccount
  mcps: McpConnection
  agents: Agent
  projects: Project
  tasks: Task
}

interface RunQuery {
  status?: string
  taskId?: string
  limit?: number
  offset?: number
}

const runListColumns = 'json_remove(data,\'$.snapshot\',\'$.summary\') AS data,json_extract(data,\'$.snapshot.task.name\') AS taskName,json_extract(data,\'$.snapshot.agent.name\') AS agentName'

function runListItem(row: Record<string, unknown>): RunListItem {
  return { ...JSON.parse(row.data as string), taskName: row.taskName, agentName: row.agentName }
}

export class Store {
  db: DatabaseSync
  constructor(directory: string) {
    mkdirSync(directory, { recursive: true, mode: 0o700 })
    this.db = new DatabaseSync(path.join(directory, 'manager.db'))
    const version = this.db.prepare('PRAGMA user_version').get()!.user_version as number
    if (version > 4) {
      this.db.close()
      throw new Error(
        'This database belongs to a newer application version. Restore a compatible backup or upgrade the application.',
      )
    }

    this.db
      .exec(`PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS records (id TEXT PRIMARY KEY,kind TEXT NOT NULL,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS records_kind ON records(kind,updated_at DESC);
      CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY,task_id TEXT NOT NULL,project_id TEXT NOT NULL,status TEXT NOT NULL,created_at INTEGER NOT NULL,data TEXT NOT NULL,dedupe TEXT UNIQUE);
      CREATE INDEX IF NOT EXISTS runs_status ON runs(status,created_at);
      CREATE INDEX IF NOT EXISTS runs_task ON runs(task_id,created_at DESC);
      CREATE INDEX IF NOT EXISTS runs_created ON runs(created_at DESC,id DESC);
      CREATE UNIQUE INDEX IF NOT EXISTS runs_active_task ON runs(task_id) WHERE status IN ('queued','running');
      CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,created_at INTEGER NOT NULL,type TEXT NOT NULL,text TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS events_run ON events(run_id,id);
      CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY,data TEXT NOT NULL,expires INTEGER);
      CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY AUTOINCREMENT,created_at INTEGER NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL);
      `)
    if (version < 2) {
      this.transaction(() => {
        this.db.exec('ALTER TABLE events ADD COLUMN payload TEXT; PRAGMA user_version=2;')
      })
    }

    if (version < 4) {
      this.transaction(() => this.db.exec(`CREATE TABLE IF NOT EXISTS chat_messages(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,data TEXT NOT NULL,created_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS chat_messages_chat ON chat_messages(chat_id,created_at); PRAGMA user_version=4;`))
    }
  }

  list<K extends keyof Records>(kind: K): Records[K][] {
    return this.db
      .prepare('SELECT data FROM records WHERE kind=? ORDER BY updated_at DESC')
      .all(kind)
      .map(r => JSON.parse(r.data as string))
  }

  get<K extends keyof Records>(kind: K, id: string): Records[K] | undefined {
    const r = this.db
      .prepare('SELECT data FROM records WHERE kind=? AND id=?')
      .get(kind, id)
    return r ? JSON.parse(r.data as string) : undefined
  }

  put<K extends keyof Records>(kind: K, value: Records[K]) {
    this.db
      .prepare(
        'INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,updated_at=excluded.updated_at',
      )
      .run(value.id, kind, JSON.stringify(value), Date.now())
    return value
  }

  remove(kind: keyof Records, id: string) {
    this.db.prepare('DELETE FROM records WHERE kind=? AND id=?').run(kind, id)
  }

  run(id: string): Run | undefined {
    const r = this.db.prepare('SELECT data FROM runs WHERE id=?').get(id)
    return r ? JSON.parse(r.data as string) : undefined
  }

  private runRows(
    {
      status,
      taskId,
      limit = 40,
      offset = 0,
    }: RunQuery = {},
    columns = 'data',
  ) {
    const where: string[] = []
    const values: (string | number)[] = []
    if (status) {
      where.push('status=?')
      values.push(status)
    }

    if (taskId) {
      where.push('task_id=?')
      values.push(taskId)
    }

    return this.db
      .prepare(
        `SELECT ${columns} FROM runs ${where.length ? `WHERE ${where.join(' AND ')}` : ''} ORDER BY created_at DESC,id DESC LIMIT ? OFFSET ?`,
      )
      .all(...values, limit, offset)
  }

  runs(query: RunQuery = {}): Run[] {
    return this.runRows(query).map(r => JSON.parse(r.data as string))
  }

  listRuns(query: RunQuery = {}): RunListItem[] {
    return this.runRows(query, runListColumns).map(runListItem)
  }

  latestTaskRuns(): RunListItem[] {
    return this.db.prepare(`SELECT ${runListColumns} FROM runs WHERE id IN (
      SELECT (SELECT id FROM runs WHERE task_id=records.id ORDER BY created_at DESC,id DESC LIMIT 1)
      FROM records WHERE kind='tasks'
    ) ORDER BY created_at DESC,id DESC`).all().map(runListItem)
  }

  maintain(now = Date.now()) {
    this.db
      .prepare('DELETE FROM kv WHERE expires IS NOT NULL AND expires <= ?')
      .run(now)
    this.db
      .prepare('DELETE FROM audit WHERE created_at < ?')
      .run(now - 90 * 86400000)
    this.db
      .prepare(
        'DELETE FROM events WHERE created_at < ? AND run_id IN (SELECT id FROM runs WHERE status NOT IN (\'queued\',\'running\') AND json_extract(data,\'$.trigger\') != \'chat\')',
      )
      .run(now - 30 * 86400000)
  }

  addRun(run: Run, dedupe: string | null = null) {
    this.db
      .prepare('INSERT INTO runs VALUES(?,?,?,?,?,?,?)')
      .run(
        run.id,
        run.taskId,
        run.projectId ?? '',
        run.status,
        run.createdAt,
        JSON.stringify(run),
        dedupe,
      )
    return run
  }

  updateRun(id: string, patch: Partial<Run>) {
    const run = this.run(id)
    if (!run)
      throw new Error('Run not found')
    Object.assign(run, patch)
    this.db
      .prepare('UPDATE runs SET status=?,data=? WHERE id=?')
      .run(run.status, JSON.stringify(run), id)
    return run
  }

  active() {
    return this.db
      .prepare(
        'SELECT data FROM runs WHERE status IN (\'queued\',\'running\') ORDER BY created_at',
      )
      .all()
      .map(r => JSON.parse(r.data as string) as Run)
  }

  events(runId: string, after = 0, limit = 100): RunEvent[] {
    return this.db
      .prepare(
        'SELECT id,run_id AS runId,created_at AS createdAt,type,text,payload FROM events WHERE run_id=? AND id>? ORDER BY id LIMIT ?',
      )
      .all(runId, after, limit)
      .map(({ payload, ...row }) => ({
        ...row,
        ...(payload ? { payload: JSON.parse(payload as string) } : {}),
      })) as unknown as RunEvent[]
  }

  event(runId: string, type: string, text: string, payload?: Record<string, unknown>) {
    this.db
      .prepare(
        'INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)',
      )
      .run(runId, Date.now(), type, text.slice(0, 16000), payload ? JSON.stringify(payload) : null)
    this.db
      .prepare(
        `DELETE FROM events WHERE run_id=? AND id < (SELECT COALESCE(MAX(id),0)-10000 FROM events WHERE run_id=?) AND run_id IN (SELECT id FROM runs WHERE json_extract(data,'$.trigger') != 'chat')`,
      )
      .run(runId, runId)
  }

  chatMessages(chatId: string): ChatMessage[] {
    return this.db.prepare('SELECT data FROM chat_messages WHERE chat_id=? ORDER BY created_at,rowid').all(chatId).map(row => JSON.parse(row.data as string))
  }

  putChatMessage(message: ChatMessage) {
    this.db.prepare('INSERT INTO chat_messages VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data').run(message.id, message.chatId, JSON.stringify(message), message.createdAt)
    return message
  }

  deleteChatMessage(chatId: string, id: string) {
    this.db.prepare('DELETE FROM chat_messages WHERE chat_id=? AND id=?').run(chatId, id)
  }

  kv<T>(key: string): T | undefined {
    const row = this.db
      .prepare(
        'SELECT data FROM kv WHERE key=? AND (expires IS NULL OR expires>?)',
      )
      .get(key, Date.now())
    return row ? JSON.parse(row.data as string) : undefined
  }

  set(key: string, data: unknown, expires: number | null = null) {
    this.db
      .prepare(
        'INSERT INTO kv VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET data=excluded.data,expires=excluded.expires',
      )
      .run(key, JSON.stringify(data), expires)
  }

  delete(key: string) {
    this.db.prepare('DELETE FROM kv WHERE key=?').run(key)
  }

  keys(prefix: string) {
    return this.db
      .prepare(
        'SELECT key,data FROM kv WHERE key LIKE ? AND (expires IS NULL OR expires>?)',
      )
      .all(`${prefix}%`, Date.now())
      .map(r => ({
        key: r.key as string,
        data: JSON.parse(r.data as string),
      }))
  }

  audit(action: string, detail: unknown) {
    this.db
      .prepare('INSERT INTO audit(created_at,action,detail) VALUES(?,?,?)')
      .run(Date.now(), action, JSON.stringify(detail))
  }

  stats() {
    const counts: Partial<Record<RunStatus, number>> = {}
    for (const r of this.db
      .prepare('SELECT status,COUNT(*) AS count FROM runs GROUP BY status')
      .all())
      counts[r.status as RunStatus] = r.count as number
    return counts
  }

  transaction<T>(fn: () => T): T {
    this.db.exec('BEGIN IMMEDIATE')
    try {
      const result = fn()
      this.db.exec('COMMIT')
      return result
    }
    catch (e) {
      this.db.exec('ROLLBACK')
      throw e
    }
  }

  close() {
    this.db.close()
  }
}
