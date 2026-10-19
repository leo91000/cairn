import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { MessageTimes } from '../src/message-times'

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllEnvs()
})

it('retains exact native labels across streamed updates and forgets messages outside the reader', () => {
  const timestamp = Date.UTC(2026, 9, 19, 12, 30)
  const label = new Date(timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  const calls = vi.spyOn(Date.prototype, 'toLocaleTimeString')
  const clock = new MessageTimes()
  clock.beginRender()
  const first = clock.format(timestamp)
  expect(first).toEqual({
    datetime: new Date(timestamp).toISOString(),
    title: new Date(timestamp).toLocaleString(),
    label,
  })
  clock.beginRender()
  expect(clock.format(timestamp)).toBe(first)
  expect(calls).toHaveBeenCalledTimes(1)

  clock.beginRender()
  clock.format(timestamp + 1)
  clock.beginRender()
  expect(clock.format(timestamp)).toEqual(first)
  expect(calls).toHaveBeenCalledTimes(3)
})

it('invalidates cached dates when the default formatting context changes', () => {
  const timestamp = Date.UTC(2026, 9, 19, 12, 30)
  const options = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' }).resolvedOptions()
  const current = { ...options }
  vi.spyOn(Intl.DateTimeFormat.prototype, 'resolvedOptions').mockImplementation(() => ({ ...current }))
  const clock = new MessageTimes()
  clock.beginRender()
  let previous = clock.format(timestamp)
  // Locale, zone and user-preferred hour/calendar settings all participate in invalidation.
  for (const change of [
    { locale: options.locale === 'fr-FR' ? 'ja-JP' : 'fr-FR' },
    { timeZone: options.timeZone === 'Pacific/Auckland' ? 'UTC' : 'Pacific/Auckland' },
    { hourCycle: options.hourCycle === 'h23' ? 'h12' as const : 'h23' as const },
    { calendar: options.calendar === 'buddhist' ? 'gregory' : 'buddhist' },
    { numberingSystem: options.numberingSystem === 'arab' ? 'latn' : 'arab' },
  ]) {
    Object.assign(current, change)
    clock.beginRender()
    const next = clock.format(timestamp)
    expect(next).not.toBe(previous)
    expect(next.title).toBe(new Date(timestamp).toLocaleString())
    expect(next.label).toBe(new Date(timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }))
    previous = next
  }
})

it('handles daylight-saving dates individually instead of reusing a timestamp offset', () => {
  const clock = new MessageTimes()
  const timestamps = [Date.UTC(2026, 2, 29, 0, 30), Date.UTC(2026, 2, 29, 1, 30), Date.UTC(2026, 9, 25, 1, 30)]
  vi.stubEnv('TZ', 'UTC')
  clock.beginRender()
  const utc = timestamps.map(timestamp => clock.format(timestamp))
  vi.stubEnv('TZ', 'Europe/Paris')
  clock.beginRender()
  expect(timestamps.map(timestamp => new Date(timestamp).getTimezoneOffset())).toEqual([-60, -120, -60])
  for (const [index, timestamp] of timestamps.entries()) {
    expect(clock.format(timestamp)).not.toBe(utc[index])
    expect(clock.format(timestamp).title).toBe(new Date(timestamp).toLocaleString())
    expect(clock.format(timestamp).label).toBe(new Date(timestamp).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }))
  }
})
