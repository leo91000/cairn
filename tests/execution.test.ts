import { execFile } from 'node:child_process'
import {
  access,
  mkdir,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import { promisify } from 'node:util'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { fixture } from './helpers.ts'
import { prepareExecution } from './legacy/server/execution.ts'

const exec = promisify(execFile)

async function git(directory: string, ...args: string[]) {
  const result = await exec('git', ['-C', directory, ...args], {
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: 'Fixture',
      GIT_AUTHOR_EMAIL: 'fixture@example.test',
      GIT_COMMITTER_NAME: 'Fixture',
      GIT_COMMITTER_EMAIL: 'fixture@example.test',
    },
  })
  return result.stdout.trim()
}

describe('isolated Git workspaces', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture({ runnerUrl: 'http://runner' })
    await mkdir(path.join(ctx.home, '.codex'))
    await writeFile(path.join(ctx.home, '.codex/auth.json'), '{"test":"login"}')
  })
  afterEach(async () => {
    await ctx.dispose()
  })

  it.each(['full', 'blob:none', 'tree:0'])('prepares an independent workspace from a %s checkout with local commits', async (filter) => {
    const upstream = path.join(ctx.directory, 'upstream')
    await mkdir(upstream)
    await git(upstream, 'init', '-b', 'main')
    await git(upstream, 'config', 'uploadpack.allowFilter', 'true')
    await git(upstream, 'config', 'uploadpack.allowAnySHA1InWant', 'true')
    await writeFile(path.join(upstream, 'example.txt'), 'historical content\n')
    await git(upstream, 'add', '.')
    await git(upstream, 'commit', '-m', 'Initial')
    const historical = await git(upstream, 'rev-parse', 'HEAD:example.txt')
    await writeFile(path.join(upstream, 'example.txt'), 'current content\n')
    await git(upstream, 'commit', '-am', 'Update')
    const url = pathToFileURL(upstream).href
    await git(ctx.directory, 'clone', ...(filter === 'full' ? [] : [`--filter=${filter}`]), url, ctx.projectPath)
    await writeFile(path.join(ctx.projectPath, 'local.txt'), 'unpublished commit\n')
    await git(ctx.projectPath, 'add', '.')
    await git(ctx.projectPath, 'commit', '-m', 'Local only')
    const head = await git(ctx.projectPath, 'rev-parse', 'HEAD')
    await writeFile(path.join(ctx.projectPath, 'example.txt'), 'uncommitted edit\n')
    if (filter !== 'full') {
      // Prove this is an incomplete object store, not merely a configured filter.
      expect(await git(ctx.projectPath, 'rev-list', '--objects', '--all', '--missing=print')).toMatch(/^\?/m)
    }

    const agent = ctx.service.agent({ name: 'Isolated', access: { skills: [], github: true } })
    const task = ctx.service.task({ ...ctx.task, agentId: agent.id, worktree: true })
    const run = await ctx.service.enqueue(task.id)

    const prepared = await prepareExecution(run, ctx.service.config)

    expect(prepared.workspaces[0].kind).toBe('clone')
    expect(await git(prepared.cwd, 'rev-parse', 'HEAD')).toBe(head)
    expect(await git(prepared.cwd, 'remote', 'get-url', 'origin')).toBe(url)
    expect(await readFile(path.join(prepared.cwd, 'example.txt'), 'utf8')).toBe('current content\n')
    expect(await readFile(path.join(prepared.cwd, 'local.txt'), 'utf8')).toBe('unpublished commit\n')
    expect(await readFile(path.join(ctx.projectPath, 'example.txt'), 'utf8')).toBe('uncommitted edit\n')
    expect(await git(ctx.projectPath, 'rev-parse', 'HEAD')).toBe(head)
    await expect(access(path.join(prepared.cwd, '.git/objects/info/alternates'))).rejects.toThrow()
    // History must remain readable with both source and upstream removed.
    await rm(ctx.projectPath, { recursive: true })
    await rm(upstream, { recursive: true })
    expect(await git(prepared.cwd, 'cat-file', '-p', historical)).toBe('historical content')
    expect(await git(prepared.cwd, 'fsck', '--full')).toBe('')
  })
})
