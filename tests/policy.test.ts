import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { MAIN_AGENT_ID } from '../shared/contracts.ts'
import { LOCAL_NODE_ID } from '../shared/nodes.ts'
import { fixture } from './helpers.ts'
import { prepareExecution } from './legacy/server/execution.ts'
import { isolated } from './legacy/server/policy.ts'
import { translateMount } from './legacy/server/runner-broker.ts'
import { codexArgs } from './legacy/server/worker.ts'

describe('agent access and task inheritance', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture()
  })
  afterEach(async () => {
    await ctx.dispose()
  })
  it('creates a default main agent and allows project-free tasks', async () => {
    const main = ctx.service.store.get('agents', MAIN_AGENT_ID)!
    expect(main.access).toEqual({ nodes: [LOCAL_NODE_ID], projects: null, skills: null, mcps: null, mcpTools: {}, github: true, sandbox: 'yolo' })
    expect(() => ctx.service.remove('agents', main.id)).toThrow(/cannot be removed/)
    const task = ctx.service.task({ name: 'Cross-project review', prompt: 'Review projects', agentId: main.id, worktree: false })
    expect(task.projectId).toBeNull()
    const secondPath = path.join(ctx.directory, 'second')
    await mkdir(secondPath)
    const second = await ctx.service.project({ name: 'Second', path: secondPath })
    const run = await ctx.service.enqueue(task.id)
    expect(run.snapshot.projects?.map(project => project.id)).toEqual(expect.arrayContaining([ctx.project.id, second.id]))
    expect(run.projectId).toBeNull()
  })
  it('enforces project and skill restrictions through task APIs', async () => {
    const agent = ctx.service.agent({ name: 'Restricted', access: { projects: [], skills: [], github: false } })
    expect(isolated(agent)).toBe(true)
    expect(() => ctx.service.task({ ...ctx.task, agentId: agent.id })).toThrow(/unavailable/)
    expect(() => ctx.service.task({ ...ctx.task, projectId: null, agentId: agent.id, skills: ['global/release'] })).toThrow(/outside/)
    expect(() => ctx.service.agent({ name: 'Leaky credentials', access: { projects: [] } })).toThrow(/GitHub/)
    const task = ctx.service.task({ name: 'No resources', prompt: 'Think', agentId: agent.id })
    const run = await ctx.service.enqueue(task.id)
    expect(run.snapshot.projects).toEqual([])
    expect(run.snapshot.skills).toEqual([])
  })
  it('rejects queued execution after permissions change without widening the snapshot', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    ctx.service.agent({ ...ctx.agent, access: { projects: [], skills: [], github: false } }, ctx.agent.id)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status).toBe('failed')
    expect(ctx.service.store.run(run.id)?.summary).toContain('access changed')
  })
  it('preserves restrictions when older API clients omit access during profile edits', () => {
    const agent = ctx.service.agent({ name: 'Restricted', access: { projects: [], skills: [], github: false, sandbox: 'read-only' } })
    const updated = ctx.service.agent({ name: 'Renamed' }, agent.id)
    expect(updated.access).toEqual(agent.access)
  })
  it('fails closed when an isolated runner is unavailable', async () => {
    const agent = ctx.service.agent({ name: 'Restricted', access: { github: false } })
    const task = ctx.service.task({ ...ctx.task, agentId: agent.id })
    const run = await ctx.service.enqueue(task.id)
    await expect(prepareExecution(run, ctx.service.config)).rejects.toThrow(/will not fall back/)
  })
  it('stages selected skill resources and login without sharing the manager home', async () => {
    await mkdir(path.join(ctx.home, '.codex'), { recursive: true })
    await writeFile(path.join(ctx.home, '.codex/auth.json'), '{"test":"login"}')
    await writeFile(path.join(ctx.home, '.codex/config.toml'), '[mcp_servers.private]\ncommand="private-tool"')
    await ctx.service.skills.save('review', '---\nname: review\ndescription: Review\n---\nReview carefully')
    await ctx.service.skills.file('review', 'references/checks.md', 'Expected checks')
    await ctx.service.skills.save('release', '---\nname: release\ndescription: Release\n---\nPublish')
    const agent = ctx.service.agent({ name: 'Reviewer', access: { projects: [ctx.project.id], skills: ['global/review'], github: false, sandbox: 'read-only' } })
    const task = ctx.service.task({ ...ctx.task, agentId: agent.id, skills: null })
    const run = await ctx.service.enqueue(task.id)
    const prepared = await prepareExecution(run, { ...ctx.service.config, runnerUrl: 'http://runner' })
    expect(prepared.mounts.find(mount => mount.source === ctx.projectPath)?.readOnly).toBe(true)
    expect(prepared.mounts.some(mount => mount.source === ctx.home)).toBe(false)
    expect(prepared.skills.map(skill => skill.name)).toEqual(['review'])
    const skillRelative = path.relative('/home/node', prepared.skills[0].path)
    const stagedHome = prepared.mounts.find(mount => mount.target === '/home/node')!.source
    expect(await readFile(path.join(stagedHome, skillRelative), 'utf8')).toContain('Review carefully')
    expect(await readFile(path.join(stagedHome, path.dirname(skillRelative), 'references/checks.md'), 'utf8')).toBe('Expected checks')
    const home = prepared.mounts.find(mount => mount.target === '/home/node')!.source
    expect(await readFile(path.join(home, '.codex/auth.json'), 'utf8')).toContain('login')
    expect(await readFile(path.join(home, '.codex/config.toml'), 'utf8')).not.toContain('private-tool')
    expect(codexArgs(run, '/tmp/result')).toEqual(expect.arrayContaining(['--sandbox', 'read-only', '-a', 'never']))
    expect(codexArgs(run, '/tmp/result')).not.toContain('--dangerously-bypass-approvals-and-sandbox')
  })
  it('stores dedicated GitHub credentials without returning them and removes them with the agent', async () => {
    const headers = await ctx.login()
    const agent = ctx.service.agent({ name: 'Private connection', access: { github: false } })
    const url = `/api/agents/${agent.id}/github-token`
    const saved = await ctx.app.inject({ method: 'PUT', url, headers, payload: { token: 'fixture-private-credential' } })
    expect(saved.statusCode).toBe(200)
    expect(saved.json()).toEqual({ configured: true })
    const read = await ctx.app.inject({ method: 'GET', url, headers })
    expect(read.json()).toEqual({ configured: true })
    const agents = await ctx.app.inject({ method: 'GET', url: '/api/agents', headers })
    expect(agents.body).not.toContain('fixture-private-credential')
    ctx.service.remove('agents', agent.id)
    expect(ctx.service.store.kv(`agent-github:${agent.id}`)).toBeUndefined()
  })
  it('translates only mount paths inside the manager volumes', () => {
    const mounts = [{ Source: '/volumes/data', Destination: '/data' }]
    expect(translateMount('/data/runs/id', mounts)).toBe('/volumes/data/runs/id')
    for (const source of ['/etc/passwd', '/data-other/secret', '/data/../etc/passwd'])
      expect(() => translateMount(source, mounts)).toThrow(/outside/)
  })
})
