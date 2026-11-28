/** A timestamp for a datetime-local input, in the given timezone. */
export function localDateTime(timestamp: number, timeZone?: string) {
  const parts = new Intl.DateTimeFormat('en-CA', {
    timeZone,
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hourCycle: 'h23',
  }).formatToParts(timestamp)
  const get = (name: Intl.DateTimeFormatPartTypes) => parts.find(part => part.type === name)?.value
  return `${get('year')}-${get('month')}-${get('day')}T${get('hour')}:${get('minute')}`
}

/** Resolve a wall-clock time in its selected IANA timezone. Reject a skipped local time. */
export function scheduledTimestamp(date: string, timeZone: string) {
  const wallTime = Date.parse(`${date}Z`)
  if (!Number.isFinite(wallTime))
    return null

  try {
    const offsets = [-86400000, 0, 86400000].map((distance) => {
      const sample = wallTime + distance
      return Date.parse(`${localDateTime(sample, timeZone)}Z`) - sample
    })
    const matches = offsets.map(offset => wallTime - offset)
      .filter(timestamp => localDateTime(timestamp, timeZone) === date)
    return matches.length ? Math.min(...matches) : null
  }
  catch {
    return null
  }
}
