import type { RunListItem, RunStatus, Task } from '../shared/contracts'

// Missions as on Android (MissionsScreen.kt, Schedule.kt): readable schedules, filters, the order
// of the list and the stamps of upcoming runs.

const weekdays: Record<string, string> = {
  0: 'Sunday',
  7: 'Sunday',
  1: 'Monday',
  2: 'Tuesday',
  3: 'Wednesday',
  4: 'Thursday',
  5: 'Friday',
  6: 'Saturday',
  SUN: 'Sunday',
  MON: 'Monday',
  TUE: 'Tuesday',
  WED: 'Wednesday',
  THU: 'Thursday',
  FRI: 'Friday',
  SAT: 'Saturday',
}

function clock(minute: string, hour: string) {
  const m = /^\d+$/.test(minute) ? Number(minute) : Number.NaN
  const h = /^\d+$/.test(hour) ? Number(hour) : Number.NaN
  if (!(m >= 0 && m <= 59 && h >= 0 && h <= 23))
    return null
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`
}

/** Human wording for the common cron shapes; anything else stays visible as the raw expression. */
export function describeCron(cron: string) {
  const source = cron.trim()
  const raw = `Cron · ${source}`
  const parts = source.split(/\s+/)
  if (parts.length !== 5)
    return raw
  const [minute, hour, day, month, weekday] = parts as [string, string, string, string, string]
  if (month !== '*')
    return raw
  if (hour === '*' && day === '*' && weekday === '*') {
    if (minute === '*')
      return 'Every minute'
    const every = minute.match(/^\*\/(\d+)$/)?.[1]
    if (every)
      return `Every ${every} minutes`
    if (/^\d+$/.test(minute))
      return minute === '0' ? 'Every hour' : `Every hour at :${minute.padStart(2, '0')}`
  }
  const time = clock(minute, hour)
  if (!time)
    return raw
  const upper = weekday.toUpperCase()
  if (day === '*' && weekday === '*')
    return `Every day · ${time}`
  if (day === '*' && ['1-5', 'MON-FRI'].includes(upper))
    return `Weekdays · ${time}`
  if (day === '*' && ['0,6', '6,0', 'SAT,SUN', '6-7'].includes(upper))
    return `Weekends · ${time}`
  if (day === '*' && weekdays[upper])
    return `Every ${weekdays[upper]} · ${time}`
  if (day === '*' && upper.split(',').every(value => weekdays[value]))
    return `${[...new Set(upper.split(',').map(value => weekdays[value]))].join(', ')} · ${time}`
  if (weekday === '*' && /^\d+$/.test(day) && Number(day) >= 1 && Number(day) <= 31)
    return `${Number(day) === 1 ? '1st' : Number(day) === 2 ? '2nd' : Number(day) === 3 ? '3rd' : `${Number(day)}th`} of the month · ${time}`
  return raw
}

export function describeSchedule(task: Pick<Task, 'archived' | 'cron' | 'enabled'>) {
  if (task.archived)
    return 'Archived'
  if (!task.cron)
    return 'One-off'
  return task.enabled ? describeCron(task.cron) : `Paused · ${describeCron(task.cron)}`
}

/** "Today · 09:00", "Tomorrow · 09:00", "Mon, Sep 29 · 09:00". */
export function upcomingStamp(value: number, now = Date.now()) {
  if (!value || value <= 0)
    return ''
  const moment = new Date(value)
  const start = (date: Date) => new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime()
  const days = Math.round((start(moment) - start(new Date(now))) / 86400000)
  const time = moment.toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit' })
  const label = days === 0
    ? 'Today'
    : days === 1
      ? 'Tomorrow'
      : moment.toLocaleDateString('en-US', { weekday: 'short', month: 'short', day: 'numeric', ...(moment.getFullYear() === new Date(now).getFullYear() ? {} : { year: 'numeric' }) })
  return `${label} · ${time}`
}

export const missionFilters = [
  { value: 'all', label: 'All' },
  { value: 'scheduled', label: 'Scheduled' },
  { value: 'once', label: 'One-off' },
  { value: 'paused', label: 'Paused' },
  { value: 'archived', label: 'Archived' },
] as const
export type MissionFilter = (typeof missionFilters)[number]['value']

export function missionFilter(task: Task, filter: MissionFilter) {
  if (filter === 'archived' ? !task.archived : task.archived)
    return false
  if (filter === 'scheduled')
    return !!task.cron && task.enabled
  if (filter === 'paused')
    return !task.enabled
  if (filter === 'once')
    return !task.cron
  return true
}

export const activeRun = (status?: RunStatus) => status === 'running' || status === 'queued'
export type MissionGroup = 'running' | 'review' | 'ready' | 'finished'
export function missionGroup(run?: Pick<RunListItem, 'status' | 'outcome'>): MissionGroup {
  if (activeRun(run?.status))
    return 'running'
  if (run && (run.status === 'failed' || run.status === 'interrupted' || (run.status === 'succeeded' && !!run.outcome && run.outcome.status !== 'completed')))
    return 'review'
  return run ? 'finished' : 'ready'
}

/** Running first, then what needs review, then the next scheduled ones, then by name. */
export function missionOrder(tasks: Task[], latest: Map<string, Pick<RunListItem, 'status' | 'outcome'>>) {
  const rank = (task: Task) => ({ running: 0, review: 1, ready: 2, finished: 2 })[missionGroup(latest.get(task.id))]
  const next = (task: Task) => task.enabled && task.nextRun !== null ? task.nextRun : Number.MAX_SAFE_INTEGER
  return tasks.toSorted((a, b) => rank(a) - rank(b) || next(a) - next(b) || a.name.toLowerCase().localeCompare(b.name.toLowerCase()))
}

/** Share of finished runs that succeeded, as a whole percentage; null before the first finished run. */
export function successRate(runs: Pick<RunListItem, 'status'>[]) {
  const finished = runs.filter(run => !activeRun(run.status))
  return finished.length ? Math.floor(finished.filter(run => run.status === 'succeeded').length * 100 / finished.length) : null
}

/** Minutes since a start, "12 min" or "2 h 05"; "< 1 min" at first. */
export function elapsedMinutes(start: number | null, now = Date.now()) {
  if (!start || start <= 0)
    return ''
  const minutes = Math.floor(Math.max(0, now - start) / 60000)
  if (minutes < 1)
    return '< 1 min'
  return minutes < 60 ? `${minutes} min` : `${Math.floor(minutes / 60)} h ${String(minutes % 60).padStart(2, '0')}`
}

export function runDuration(run: Pick<RunListItem, 'startedAt' | 'finishedAt'>, now = Date.now()) {
  if (!run.startedAt)
    return '—'
  const seconds = Math.floor(Math.max(0, (run.finishedAt ?? now) - run.startedAt) / 1000)
  return `${Math.floor(seconds / 60)} min ${seconds % 60} s`
}
