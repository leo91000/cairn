import type { RunListItem, Task } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import {
  describeCron,
  describeSchedule,
  missionFilter,
  missionOrder,
  successRate,
  upcomingStamp,
} from '../src/missions'

function task(id: string, extra: Partial<Task> = {}): Task {
  return {
    id,
    name: id,
    prompt: 'Do it',
    agentId: 'main',
    projectId: null,
    skills: null,
    tags: [],
    cron: null,
    timezone: 'Europe/Paris',
    enabled: true,
    archived: false,
    worktree: true,
    createdAt: 0,
    nextRun: null,
    ...extra,
  }
}

describe('missions', () => {
  it('words common schedules and keeps anything else visible as cron', () => {
    expect(describeCron('0 9 * * 1')).toBe('Every Monday · 09:00')
    expect(describeCron('30 8 * * 1-5')).toBe('Weekdays · 08:30')
    expect(describeCron('0 18 * * *')).toBe('Every day · 18:00')
    expect(describeCron('*/15 * * * *')).toBe('Every 15 minutes')
    expect(describeCron('5 * * * *')).toBe('Every hour at :05')
    expect(describeCron('0 7 1 * *')).toBe('1st of the month · 07:00')
    expect(describeCron('0 9 * 1 *')).toBe('Cron · 0 9 * 1 *')
    expect(describeSchedule(task('a', { cron: '0 9 * * 1', enabled: false }))).toBe('Paused · Every Monday · 09:00')
    expect(describeSchedule(task('a'))).toBe('One-off')
  })

  it('stamps upcoming runs relative to today', () => {
    const now = new Date(2026, 8, 28, 10, 0).getTime()
    expect(upcomingStamp(new Date(2026, 8, 28, 18, 30).getTime(), now)).toBe('Today · 18:30')
    expect(upcomingStamp(new Date(2026, 8, 29, 9, 0).getTime(), now)).toBe('Tomorrow · 09:00')
    expect(upcomingStamp(new Date(2026, 9, 6, 9, 0).getTime(), now)).toBe('Tue, Oct 6 · 09:00')
  })

  it('filters and orders missions: running, then review, then the next scheduled, then by name', () => {
    const tasks = [task('zeta'), task('later', { cron: '0 9 * * 1', nextRun: 2000 }), task('sooner', { cron: '0 9 * * 1', nextRun: 1000 }), task('broken'), task('busy'), task('old', { archived: true, enabled: false })]
    const latest = new Map<string, Pick<RunListItem, 'status' | 'outcome'>>([['busy', { status: 'running' }], ['broken', { status: 'failed' }]])
    expect(missionOrder(tasks.filter(item => missionFilter(item, 'all')), latest).map(item => item.id)).toEqual(['busy', 'broken', 'sooner', 'later', 'zeta'])
    expect(tasks.filter(item => missionFilter(item, 'scheduled')).map(item => item.id)).toEqual(['later', 'sooner'])
    expect(tasks.filter(item => missionFilter(item, 'archived')).map(item => item.id)).toEqual(['old'])
    expect(successRate([{ status: 'succeeded' }, { status: 'failed' }, { status: 'succeeded' }, { status: 'running' }])).toBe(66)
    expect(successRate([{ status: 'queued' }])).toBeNull()
  })
})
