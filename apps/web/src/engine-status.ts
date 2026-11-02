import type { EngineRetry } from '../../../packages/contracts/generated/typescript/EngineRetry'
import type { UsageLimit } from '../../../packages/contracts/generated/typescript/UsageLimit'

/**
 * How the conversation words what the coding engine reports about its own requests: retries
 * it makes itself, the provider's usage limit, and its notices. The server interprets them;
 * these only localize its codes and arguments.
 */
const reasons: Record<string, string> = {
  rate_limit: 'Rate limited',
  overloaded: 'Servers overloaded',
  server_error: 'Server error',
  authentication_failed: 'Authentication failed',
  billing_error: 'Billing error',
  invalid_request: 'Invalid request',
  model_not_found: 'Model not found',
  max_output_tokens: 'Output limit reached',
}

const windows: Record<string, string> = {
  five_hour: '5-hour limit',
  seven_day: 'weekly limit',
  seven_day_opus: 'weekly Opus limit',
  seven_day_sonnet: 'weekly Sonnet limit',
  seven_day_overage_included: 'weekly limit',
  overage: 'extra usage limit',
}

function seconds(ms: number) {
  return ms < 1000 ? 'under a second' : ms < 90_000 ? `${Math.round(ms / 1000)} s` : `${Math.round(ms / 60_000)} min`
}

export function engineRetryTitle(retry: EngineRetry) {
  const progress = retry.attempt === null ? '' : retry.maxRetries ? ` · attempt ${retry.attempt}/${retry.maxRetries}` : ` · attempt ${retry.attempt}`
  return `Claude Code is retrying the request${progress}`
}

export function engineRetryDetail(retry: EngineRetry) {
  return [
    retry.noResponse ? 'No response from the API' : retry.reason ? reasons[retry.reason] : undefined,
    retry.httpStatus ? `HTTP ${retry.httpStatus}` : '',
    retry.delayMs === null ? '' : `next attempt after ${seconds(retry.delayMs)}`,
  ].filter(Boolean).join(' · ')
}

/** When a limit resets, in the reader's time, with the day once it is not today. */
export function limitReset(resetsAt: number | null | undefined, now = Date.now()) {
  if (!resetsAt || !Number.isFinite(resetsAt) || resetsAt < 0)
    return ''
  const date = new Date(resetsAt)
  const time = date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  return date.toDateString() === new Date(now).toDateString() ? time : `${date.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' })} ${time}`
}

export function usageLimitTitle(limit: UsageLimit) {
  const window = limit.window ? windows[limit.window] : undefined
  if (limit.status === 'allowed')
    return 'Usage limit no longer applies'
  if (limit.status === 'allowed_warning')
    return `Approaching the ${window ?? 'usage limit'}`
  const name = window ? `${window[0]!.toUpperCase()}${window.slice(1)}` : 'Usage limit'
  return limit.blocked ? `${name} reached` : `${name} reached · continuing on extra usage`
}

export function usageLimitDetail(limit: UsageLimit, now = Date.now()) {
  const reset = limitReset(limit.resetsAt, now)
  return reset ? `Resets at ${reset}` : ''
}

/** A limit no longer applies once its window resets, whatever the reader's clock says later. */
export function limitApplies(limit: UsageLimit, now = Date.now()) {
  return limit.status !== 'allowed' && !((limit.resetsAt ?? Infinity) <= now)
}

export function engineNoticeTitle(level: string | undefined) {
  return level === 'warning' ? 'Claude Code warning' : level === 'suggestion' ? 'Claude Code suggestion' : 'Claude Code notice'
}
