import { expect, it } from 'vitest'
import { performancePreference } from '../src/client-performance-preference'

it('retains explicit opt-out with missing, stale or throwing persistence', () => {
  for (const stored of [undefined, true, 'throws'] as const) {
    const preference = performancePreference({
      read: () => {
        if (stored === 'throws')
          throw new Error('storage unavailable')
        return stored
      },
      write: () => { throw new Error('storage unavailable') },
    })
    expect(preference.refresh()).toBe(true)
    expect(preference.set(false)).toBe(false)
    expect(preference.refresh()).toBe(false)
    expect(preference.refresh()).toBe(false)
    expect(preference.set(true)).toBe(true)
  }
})

it('adopts other tabs preferences after successful persistence', () => {
  let stored: boolean | undefined
  const preference = performancePreference({ read: () => stored, write: value => stored = value })
  expect(preference.refresh()).toBe(true)
  expect(preference.set(false)).toBe(false)
  stored = true
  expect(preference.refresh()).toBe(true)
  stored = false
  expect(preference.refresh()).toBe(false)
})
