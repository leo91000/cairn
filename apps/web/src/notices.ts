import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineNotice } from '../../../packages/contracts/generated/typescript/TimelineNotice'
import type { TimelineEntry } from '../../../packages/contracts/live'
import type { ActivityArtifact } from './activity'
import {
  engineNoticeTitle,
  engineRetryDetail,
  engineRetryTitle,
  limitApplies,
  usageLimitDetail,
  usageLimitTitle,
} from './engine-status'

const statusTitles: Record<string, string> = {
  succeeded: 'Run succeeded',
  failed: 'Run failed',
  cancelled: 'Run cancelled',
  interrupted: 'Run interrupted',
}

const failureTitles: Record<NonNullable<TimelineNotice['origin']>, string> = {
  turn: 'The agent ran into an error',
  run: 'Run failed',
  engine: 'Worker reported an error',
  runner: 'Runner failed',
}

function readable(value: string) {
  return value.replaceAll(/[._]/g, ' ').replace(/^./, letter => letter.toUpperCase())
}

function failureTitle(notice: TimelineNotice) {
  if (notice.retry) {
    return notice.retry.limit === null
      ? 'Temporary error — retrying automatically'
      : `Temporary error — automatic retry ${notice.retry.attempt}/${notice.retry.limit}`
  }

  if (notice.retried)
    return `The agent ran into an error after ${notice.retried} automatic ${notice.retried === 1 ? 'retry' : 'retries'}`
  return failureTitles[notice.origin ?? 'turn']
}

/** The words and facts of a notice card, without the folding the server already did. */
function presentation(notice: TimelineNotice): Pick<ActivityArtifact, 'title' | 'subtitle' | 'statusLabel' | 'verbatim'> {
  switch (notice.code) {
    case 'sessionStarted':
      return { title: 'Session connected', subtitle: '' }
    case 'turnStarted':
      return { title: 'Started working', subtitle: '' }
    case 'turnCompleted':
      return {
        title: 'Finished this turn',
        subtitle: notice.usage ? `${notice.usage.inputTokens.toLocaleString()} input · ${notice.usage.outputTokens.toLocaleString()} output tokens` : '',
      }
    case 'backgroundTasks':
      return { title: notice.tasks?.length ? 'Waiting for background tasks' : 'Background tasks finished', subtitle: notice.tasks?.join(' · ') ?? '' }
    case 'runStatus': {
      const status = notice.status ?? ''
      return { title: statusTitles[status] ?? (status.includes(' ') ? status : readable(status)), subtitle: '' }
    }

    case 'accountSelected':
      return { title: `Using ${notice.account?.agent} account: ${notice.account?.name}`, subtitle: '' }
    case 'speed':
      return notice.speed === 'standard'
        ? { title: 'This turn runs at Standard speed', subtitle: 'Codex doesn’t list the selected speed for this model here, so Cairn asks for Standard' }
        : notice.speed === 'confirmed'
          ? { title: 'Selected speed requested for this turn', subtitle: 'Codex lists it for this model here; the account’s plan decides whether it applies' }
          : { title: 'Speed not confirmed for this turn', subtitle: 'It couldn’t be confirmed for this account and model; this turn may run at Standard' }
    case 'failure':
      return { title: failureTitle(notice), subtitle: '' }
    case 'error':
      return { title: 'Worker reported an error', subtitle: '' }
    case 'connection':
      return notice.recovered
        ? { title: 'Connection interrupted', subtitle: 'Connection restored; work continued', statusLabel: 'Recovered' }
        : { title: 'Connection interrupted', subtitle: '' }
    case 'retryScheduled':
      return {
        title: 'Retry scheduled',
        subtitle: notice.retry ? `Automatic retry ${notice.retry.attempt}${notice.retry.limit === null ? '' : `/${notice.retry.limit}`}` : '',
      }
    case 'engineRetry':
      return {
        title: notice.engineRetry ? engineRetryTitle(notice.engineRetry) : 'Retrying the request',
        subtitle: notice.engineRetry ? engineRetryDetail(notice.engineRetry) : '',
        ...(notice.recovered ? { statusLabel: 'Recovered' } : {}),
      }
    case 'usageLimit':
      return {
        title: notice.usageLimit ? usageLimitTitle(notice.usageLimit) : 'Usage limit',
        subtitle: notice.usageLimit ? usageLimitDetail(notice.usageLimit) : '',
      }
    case 'engineNotice':
      return { title: engineNoticeTitle(notice.level), subtitle: '' }
    case 'diagnostic':
      return { title: 'Agent diagnostic', subtitle: '', verbatim: true }
  }
}

/** Whether a conversation shows the card now; a usage limit stops applying once it resets. */
export function noticeShown(notice: TimelineNotice, now = Date.now()) {
  if (notice.code === 'usageLimit' && notice.usageLimit)
    return notice.prominent && limitApplies(notice.usageLimit, now)
  return notice.prominent
}

/**
 * A notice card as an activity row. The run log shows every card; a conversation shows the
 * prominent ones. `source` is the raw event, kept only for the expandable record.
 */
export function noticeArtifact(card: TimelineEntry, notice: TimelineNotice, source?: RunEvent): ActivityArtifact {
  const process = notice.code === 'diagnostic'
  return {
    kind: 'notice',
    id: card.id,
    sourceIds: [`${card.runId}:${card.position}`],
    time: card.createdAt,
    status: notice.state === 'error' ? 'error' : notice.state === 'info' ? 'info' : notice.state === 'running' ? 'running' : 'done',
    blocks: notice.detail ? [{ label: process ? 'Output' : 'Details', code: notice.detail, language: 'plaintext' }] : [],
    files: [],
    tasks: [],
    raw: source ? source.payload ? JSON.stringify(source.payload, null, 2) : source.text : notice.detail ?? '',
    attention: noticeShown(notice),
    ...presentation(notice),
  }
}
