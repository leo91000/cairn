import { execFile } from 'node:child_process'
import {
  mkdir,
  readFile,
  symlink,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import { promisify } from 'node:util'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { fixture } from './helpers.ts'
import { nextOccurrences } from './legacy/server/service.ts'
import { boundedPath } from './legacy/server/skills.ts'

const skill
  = '---\nname: review\ndescription: Review a project\n---\nRun the relevant checks.\n'

describe('tasks, schedules, and skills', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture()
  })
  afterEach(async () => {
    await ctx.dispose()
  })

  it('queues one immutable snapshot despite simultaneous enqueue requests', async () => {
    await ctx.service.skills.save('review', skill)
    const task = ctx.service.task(
      { ...ctx.task, skills: ['global/review'] },
      ctx.task.id,
    )
    const results = await Promise.allSettled([
      ctx.service.enqueue(task.id),
      ctx.service.enqueue(task.id),
    ])
    expect(results.filter(item => item.status === 'fulfilled')).toHaveLength(
      1,
    )
    expect(results.filter(item => item.status === 'rejected')).toHaveLength(
      1,
    )
    ctx.service.task({ ...task, prompt: 'Changed after queueing' }, task.id)
    await ctx.service.skills.save(
      'review',
      skill.replace('relevant', 'different'),
    )
    const run = ctx.service.store.active()[0]
    expect(run.snapshot.task.prompt).toBe(ctx.task.prompt)
    expect(run.snapshot.skills[0].content).toBe(skill)
    expect(() => ctx.service.remove('tasks', task.id)).toThrow(/active work/)
    expect(() => ctx.service.remove('agents', ctx.agent.id)).toThrow(
      /used by a task/,
    )
  })

  it('catches up once after downtime and skips overlapping occurrences', async () => {
    const now = Date.parse('2026-09-09T12:00:00Z')
    const task = ctx.service.task(
      { ...ctx.task, cron: '0 9 * * *', timezone: 'UTC' },
      ctx.task.id,
    )
    ctx.service.store.put('tasks', { ...task, nextRun: now - 10 * 86400000 })
    await ctx.service.schedule(now)
    await ctx.service.schedule(now)
    expect(ctx.service.store.active()).toHaveLength(1)
    expect(ctx.service.store.get('tasks', task.id)?.nextRun).toBe(
      Date.parse('2026-09-10T09:00:00Z'),
    )
    await ctx.service.schedule(now + 86400000)
    expect(ctx.service.store.active()).toHaveLength(1)
    expect(ctx.service.store.get('tasks', task.id)?.nextRun).toBe(
      Date.parse('2026-09-11T09:00:00Z'),
    )
  })

  it('pauses schedules and does not hot-loop an invalid skill every second', async () => {
    const task = ctx.service.task(
      { ...ctx.task, cron: '* * * * *', skills: ['global/missing'] },
      ctx.task.id,
    )
    const now = task.nextRun!
    await ctx.service.schedule(now)
    expect(ctx.service.store.get('tasks', task.id)!.nextRun).toBeGreaterThan(
      now,
    )
    const paused = ctx.service.task({ ...task, enabled: false }, task.id)
    expect(paused.nextRun).toBeNull()
    await ctx.service.schedule(now + 86400000)
    expect(ctx.service.store.active()).toHaveLength(0)
    expect(() =>
      ctx.service.task({ ...task, enabled: false, cron: 'bad' }, task.id),
    ).toThrow(/valid/)
  })

  it('computes wall-clock schedules through spring and autumn DST changes', () => {
    const spring = nextOccurrences(
      '0 9 * * *',
      'Europe/Paris',
      Date.parse('2026-03-28T09:00:00Z'),
    )
    expect(new Date(spring[0]).toISOString()).toBe('2026-03-29T07:00:00.000Z')
    const autumn = nextOccurrences(
      '0 9 * * *',
      'Europe/Paris',
      Date.parse('2026-10-24T09:00:00Z'),
    )
    expect(new Date(autumn[0]).toISOString()).toBe('2026-10-25T08:00:00.000Z')
    for (const [cron, timezone] of [
      ['* * * * * *', 'UTC'],
      ['* * * * *', 'Mars/Olympus'],
      ['nope', 'UTC'],
    ])
      expect(() => nextOccurrences(cron, timezone)).toThrow(/valid/)
  })

  it('rejects missing projects and paths outside the configured roots', async () => {
    await expect(
      ctx.service.project({ name: 'Outside', path: '/tmp' }),
    ).rejects.toThrow(/root/)
    await expect(
      ctx.service.project({
        name: 'Missing',
        path: path.join(ctx.directory, 'missing'),
      }),
    ).rejects.toThrow(/exist/)
    await symlink('/tmp', path.join(ctx.directory, 'escape'))
    await expect(
      ctx.service.project({
        name: 'Escape',
        path: path.join(ctx.directory, 'escape'),
      }),
    ).rejects.toThrow(/root/)
  })

  it('refreshes Git origin metadata without storing URL credentials or query tokens', async () => {
    const exec = promisify(execFile)
    await exec('git', ['init', ctx.projectPath])
    await exec('git', ['-C', ctx.projectPath, 'remote', 'add', 'origin', 'https://user:secret@example.com/team/repo.git?token=secret#secret'])
    const saved = await ctx.service.project(ctx.project, ctx.project.id)
    expect(saved.origin).toBe('https://example.com/team/repo.git')
    await exec('git', ['-C', ctx.projectPath, 'remote', 'set-url', 'origin', 'git@example.com:team/repo.git'])
    expect((await ctx.service.project(saved, saved.id)).origin).toBe('example.com:team/repo.git')
  })

  it('edits actual global and project skills and nested supporting files', async () => {
    await ctx.service.skills.save('review', skill)
    await ctx.service.skills.save(
      'review',
      skill.replace('relevant', 'project'),
      ctx.projectPath,
    )
    await ctx.service.skills.file(
      'review',
      'references/checks.md',
      'Run unit tests',
    )
    expect(
      await ctx.service.skills.file(
        'review',
        'references/checks.md',
        undefined,
      ),
    ).toBe('Run unit tests')
    expect(await ctx.service.skills.files('review')).toEqual(
      expect.arrayContaining(['SKILL.md', 'references/checks.md']),
    )
    expect((await ctx.service.skills.list())[0].content).toBe(skill)
    expect(
      (await ctx.service.skills.list(ctx.project.id, ctx.projectPath))[0]
        .content,
    ).toContain('project checks')
    await expect(
      ctx.service.skills.save('review', 'No frontmatter'),
    ).rejects.toThrow(/frontmatter/)
    await expect(
      ctx.service.skills.save(
        'review',
        skill.replace('name: review', 'name: other'),
      ),
    ).rejects.toThrow(/match/)
    expect(
      await readFile(
        path.join(ctx.home, '.agents/skills/review/SKILL.md'),
        'utf8',
      ),
    ).toBe(skill)
  })

  it('blocks traversal and symlink escapes, including the skill root itself', async () => {
    await ctx.service.skills.save('review', skill)
    const root = path.join(ctx.home, '.agents/skills/review')
    for (const file of [
      '../secret',
      '/tmp/file',
      'references/../../secret',
      'a\\..\\secret',
    ]) {
      await expect(
        ctx.service.skills.file('review', file, 'bad'),
      ).rejects.toThrow(/path/)
    }

    await symlink(ctx.projectPath, path.join(root, 'outside'))
    await writeFile(path.join(ctx.projectPath, 'secret'), 'unchanged')
    await expect(
      ctx.service.skills.file('review', 'outside/secret', 'bad'),
    ).rejects.toThrow(/escapes/)
    await expect(
      boundedPath(root, 'outside/new', { missing: true }),
    ).rejects.toThrow(/escapes/)
    await mkdir(path.join(ctx.projectPath, '.agents'))
    await symlink(root, path.join(ctx.projectPath, '.agents/skills'))
    await expect(
      ctx.service.skills.save('review', skill, ctx.projectPath),
    ).rejects.toThrow(/symbolic/)
    expect(await readFile(path.join(ctx.projectPath, 'secret'), 'utf8')).toBe(
      'unchanged',
    )
  })
})
