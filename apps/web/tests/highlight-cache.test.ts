import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import * as highlighting from '../src/highlight'
import { HighlightCache } from '../src/highlight-cache'

afterEach(() => vi.restoreAllMocks())

it('reuses only identical code and language, preserving highlighting and escaping', () => {
  const render = vi.spyOn(highlighting, 'highlight')
  const cache = new HighlightCache()
  const code = 'const value = "<script>"'
  const first = cache.render(code, 'javascript')
  cache.beginRender()
  expect(cache.render(code, 'javascript')).toBe(first)
  expect(render).toHaveBeenCalledTimes(1)
  expect(cache.render(code, 'plaintext')).toBe('const value = "&lt;script&gt;"')
  expect(cache.render('const value = 2', 'javascript')).toBe(highlighting.highlight('const value = 2', 'javascript'))
  expect(cache.render('<img onerror=alert(1)>', 'unknown')).toBe('&lt;img onerror=alert(1)&gt;')
})

it('bounds retained entries and does not retain oversized highlighted HTML', () => {
  const render = vi.spyOn(highlighting, 'highlight')
  const cache = new HighlightCache()
  cache.render('old', 'plaintext')
  cache.beginRender()
  for (let index = 0; index < 100; index++)
    cache.render(`snippet ${index}`, 'plaintext')
  render.mockClear()
  cache.beginRender()
  cache.render('old', 'plaintext')
  expect(render).toHaveBeenCalledTimes(1)

  const oversized = '<'.repeat(65_000)
  cache.render(oversized)
  cache.render(oversized)
  expect(render).toHaveBeenCalledTimes(3)
})

it('retains useful prefix hits across repeated scans larger than its entry budget', () => {
  const render = vi.spyOn(highlighting, 'highlight')
  const cache = new HighlightCache()
  const snippets = Array.from({ length: 150 }, (_, index) => `snippet ${index}`)
  cache.beginRender()
  for (const source of snippets)
    cache.render(source)
  render.mockClear()
  cache.beginRender()
  for (const source of snippets)
    cache.render(source)
  expect(render.mock.calls.length).toBeLessThan(snippets.length)
  expect(render.mock.calls.length).toBeGreaterThan(0)
})
