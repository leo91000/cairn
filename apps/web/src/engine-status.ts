/**
 * What the coding engine reports about its own requests: retries it makes itself, the provider's
 * usage limit, and its notices. None of them is a failure of the turn, nor a wait Cairn manages.
 */
type Data = Record<string, unknown>

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

function count(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : undefined
}

function seconds(ms: number) {
  return ms < 1000 ? 'under a second' : ms < 90_000 ? `${Math.round(ms / 1000)} s` : `${Math.round(ms / 60_000)} min`
}

export function engineRetryTitle(data: Data) {
  const attempt = count(data.attempt)
  const limit = count(data.maxRetries)
  const progress = attempt === undefined ? '' : limit ? ` · attempt ${attempt}/${limit}` : ` · attempt ${attempt}`
  return `Claude Code is retrying the request${progress}`
}

export function engineRetryDetail(data: Data) {
  const reason = typeof data.reason === 'string' ? reasons[data.reason] : undefined
  const status = count(data.status)
  const noResponse = data.noResponse && typeof data.noResponse === 'object'
  const delay = count(data.delayMs)
  return [
    noResponse ? 'No response from the API' : reason,
    status ? `HTTP ${status}` : '',
    delay === undefined ? '' : `next attempt after ${seconds(delay)}`,
  ].filter(Boolean).join(' · ')
}

/** When a limit resets, in the reader's time, with the day once it is not today. */
export function limitReset(resetsAt: unknown, now = Date.now()) {
  const reset = count(resetsAt)
  if (!reset)
    return ''
  const date = new Date(reset)
  const time = date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  return date.toDateString() === new Date(now).toDateString() ? time : `${date.toLocaleDateString(undefined, { weekday: 'short', day: 'numeric', month: 'short' })} ${time}`
}

export function usageLimitTitle(data: Data) {
  const window = typeof data.window === 'string' ? windows[data.window] : undefined
  if (data.status === 'allowed')
    return 'Usage limit no longer applies'
  if (data.status === 'allowed_warning')
    return `Approaching the ${window ?? 'usage limit'}`
  if (data.blocked !== true)
    return `${window ? `${window[0]!.toUpperCase()}${window.slice(1)}` : 'Usage limit'} reached · continuing on extra usage`
  return `${window ? `${window[0]!.toUpperCase()}${window.slice(1)}` : 'Usage limit'} reached`
}

export function usageLimitDetail(data: Data, now = Date.now()) {
  const reset = limitReset(data.resetsAt, now)
  return reset ? `Resets at ${reset}` : ''
}

export function engineNoticeTitle(data: Data) {
  return data.level === 'warning' ? 'Claude Code warning' : data.level === 'suggestion' ? 'Claude Code suggestion' : 'Claude Code notice'
}

/**
 * Whether a conversation shows the event as a notice. A limit that stops requests leaves it to the
 * usage-limit banner once the turn fails; informational notices stay in the run log.
 */
export function engineAttention(type: string, data: Data) {
  if (type === 'engine.retry')
    return true
  if (type === 'provider.limit')
    return data.status !== 'allowed'
  if (type === 'engine.notice')
    return data.level === 'warning' || data.level === 'suggestion' || data.stopsTurn === true
  return false
}
