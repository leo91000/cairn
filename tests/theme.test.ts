import { readFileSync } from 'node:fs'
import { runInNewContext } from 'node:vm'
import { describe, expect, it } from 'vitest'

const script = readFileSync(new URL('../public/theme.js', import.meta.url), 'utf8')

function bootstrap(stored: string | null = null, dark = false, blocked = false) {
  const root = { dataset: {} as Record<string, string>, style: {} as Record<string, string> }
  const listeners: Record<string, (event?: any) => void> = {}
  const media = { matches: dark, addEventListener: (_: string, callback: () => void) => listeners.system = callback }
  let value = stored
  let color = ''
  const window: any = { matchMedia: () => media, addEventListener: (name: string, callback: () => void) => listeners[name] = callback }
  runInNewContext(script, {
    window,
    document: { documentElement: root, querySelector: () => ({ setAttribute: (_: string, value: string) => color = value }) },
    localStorage: {
      getItem: () => {
        if (blocked)
          throw new Error('Storage blocked')
        return value
      },
      setItem: (_: string, next: string) => {
        if (blocked)
          throw new Error('Storage blocked')
        value = next
      },
    },
  })
  return {
    root,
    media,
    listeners,
    controller: window.leoTheme,
    stored: () => value,
    color: () => color,
  }
}

describe('theme bootstrap', () => {
  it('applies the preferred scheme before the application starts', () => {
    const page = bootstrap(null, true)
    expect(page.root.dataset).toEqual({ theme: 'dark', themePreference: 'system' })
    expect(page.root.style.colorScheme).toBe('dark')
    expect(page.color()).toBe('#0f0f14')
    expect(bootstrap('light', true).root.dataset.theme).toBe('light')
    expect(bootstrap('invalid', true).root.dataset.themePreference).toBe('system')
  })
  it('reacts to system changes but retains an explicit user choice', () => {
    const page = bootstrap(null, false)
    page.media.matches = true
    page.listeners.system()
    expect(page.root.dataset.theme).toBe('dark')
    page.controller.setPreference('light')
    expect(page.stored()).toBe('light')
    page.listeners.system()
    expect(page.root.dataset.theme).toBe('light')
    page.controller.setPreference('system')
    expect(page.root.dataset.theme).toBe('dark')
  })
  it('synchronizes other tabs and reset-to-system without changing unrelated storage', () => {
    const page = bootstrap('light', true)
    let selected = ''
    const unsubscribe = page.controller.subscribe((value: string) => selected = value)
    page.listeners.storage({ key: 'leo.theme', newValue: 'dark' })
    expect(selected).toBe('dark')
    expect(page.root.dataset.theme).toBe('dark')
    page.listeners.storage({ key: 'other', newValue: 'light' })
    expect(selected).toBe('dark')
    page.listeners.storage({ key: null, newValue: null })
    expect(selected).toBe('system')
    unsubscribe()
    page.controller.setPreference('light')
    expect(selected).toBe('system')
  })
  it('still works when browser privacy settings block storage', () => {
    const page = bootstrap(null, true, true)
    expect(page.root.dataset.theme).toBe('dark')
    expect(() => page.controller.setPreference('light')).not.toThrow()
    expect(page.root.dataset.theme).toBe('light')
  })
})
