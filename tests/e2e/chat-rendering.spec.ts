import type {} from '../../scripts/fixtures/chat-rendering'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { createServer } from 'vite'

let server: Awaited<ReturnType<typeof createServer>>
let address: string

// First navigation compiles the real Markdown, syntax grammars and stylesheet graph.
test.setTimeout(60_000)

test.beforeAll(async () => {
  server = await createServer({
    configFile: false,
    root: process.cwd(),
    plugins: [vue(), tailwindcss()],
    server: { host: '127.0.0.1', port: 0 },
    logLevel: 'error',
  })
  await server.listen()
  address = server.resolvedUrls!.local[0]!
})
test.afterAll(async () => server?.close())
test.beforeEach(async ({ page }) => {
  await page.goto(`${address}scripts/fixtures/chat-rendering.html`)
  await page.waitForFunction(() => window.chatRendering)
})

test('streaming retains unchanged paragraphs, code, table position and text selection', async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 844 })
  const prefix = '# Heading\n\nA **stable paragraph** with a [link](https://example.test/docs).\n\n```typescript\nconst value = 1\n```\n\n| A | B | C | D |\n| --- | --- | --- | --- |\n| one | two | three | four |\n\n'
  await page.evaluate(text => window.chatRendering.markdown(text), `${prefix}Tail`)
  const paragraph = await page.locator('#app .markdown p').first().elementHandle()
  const code = await page.locator('#app .markdown pre').elementHandle()
  const table = await page.locator('#app .markdown table').elementHandle()
  const scrollLeft = await table!.evaluate((element) => {
    element.parentElement!.scrollLeft = 80
    return element.parentElement!.scrollLeft
  })
  expect(scrollLeft).toBeGreaterThan(0)
  await paragraph!.evaluate((element) => {
    const range = document.createRange()
    range.selectNodeContents(element)
    getSelection()!.removeAllRanges()
    getSelection()!.addRange(range)
  })
  const selection = await page.evaluate(() => getSelection()!.toString())
  await page.evaluate(text => window.chatRendering.markdown(text), `${prefix}Tail completed`)
  expect(await paragraph!.evaluate(element => element.isConnected)).toBe(true)
  expect(await code!.evaluate(element => element.isConnected)).toBe(true)
  expect(await table!.evaluate(element => element.isConnected)).toBe(true)
  expect(await table!.evaluate(element => element.parentElement!.scrollLeft)).toBe(scrollLeft)
  expect(await page.evaluate(() => getSelection()!.toString())).toBe(selection)
  await expect(page.locator('#app .markdown')).toContainText('Tail completed')
  await expect(page.locator('#app .markdown a')).toHaveAttribute('target', '_blank')
  await expect(page.locator('#app .markdown a')).toHaveAttribute('rel', 'noopener noreferrer')
  await page.locator('#app .markdown a').focus()
  await page.evaluate(text => window.chatRendering.markdown(text), `${prefix}Another fragment`)
  await expect(page.locator('#app .markdown a')).toBeFocused()
})

test('streamed corrections match a fresh full render including references, nested HTML and incomplete syntax', async ({ page }) => {
  const cases = [
    '[Documentation][ref]\n\nA paragraph.',
    '[Documentation][ref]\n\nA paragraph.\n\n[ref]: https://example.test/first',
    '[Documentation][ref]\n\nA paragraph.\n\n[ref]: https://example.test/second',
    '<div>\n\n**Nested** content\n\n</div>\n\nLast paragraph',
    '3. Three\n4. Four\n   - Child\n\n> Quote\n> continued',
    '```typescript\nconst value = "<script>"',
    '```typescript\nconst value = "changed"\n```\n\n| A | B |\n| --- | --- |\n| **one** | ~~two~~ |',
    '<script>alert("unsafe")</script><img src=x onerror="alert(1)">\n\n[unsafe](javascript:alert(1))',
    'A complete replacement.',
    '',
  ]
  for (const source of cases) {
    await page.evaluate(text => window.chatRendering.markdown(text), source)
    const actual = await page.locator('#app .markdown').innerHTML()
    const fresh = await page.evaluate(text => window.chatRendering.freshMarkdownHtml(text), source)
    expect(actual).toBe(fresh)
    await expect(page.locator('#app .markdown script, #app .markdown [onerror], #app .markdown [href^="javascript:"]')).toHaveCount(0)
  }
})

test('compact links and code language updates invalidate only their affected output', async ({ page }) => {
  const source = 'https://github.com/owner/repo/commit/abc\n\n```plaintext\nconst value = 1\n```'
  for (const compact of [false, true, false]) {
    for (const text of [source, source.replace('plaintext', 'typescript')]) {
      await page.evaluate(({ text, compact }) => window.chatRendering.markdown(text, compact), { text, compact })
      const fresh = await page.evaluate(({ text, compact }) => window.chatRendering.freshMarkdownHtml(text, compact), { text, compact })
      expect(await page.locator('#app .markdown').innerHTML()).toBe(fresh)
    }
  }
})

test('raw HTML retains parser normalization for SVG, MathML and raw text containers', async ({ page }) => {
  const content = '<svg width="600" height="100"><text x="0" y="30">/api/runs/r/artifacts/a</text></svg>\n\n<math><mtext>/api/runs/r/artifacts/a</mtext></math>\n\n<textarea>/api/runs/r/artifacts/a</textarea>\n\n<div><style>/api/runs/r/artifacts/a</style></div>'
  for (const text of [content, `${content}\n\nAnother fragment`]) {
    await page.evaluate(source => window.chatRendering.markdown(source, true), text)
    const svg = page.locator('#app .markdown svg a')
    expect(await svg.evaluate(element => element.namespaceURI)).toBe('http://www.w3.org/2000/svg')
    expect((await svg.boundingBox())!.width).toBeGreaterThan(0)
    expect(await page.locator('#app .markdown math a').evaluate(element => element.namespaceURI)).toBe('http://www.w3.org/1999/xhtml')
    expect(await page.locator('#app .markdown textarea').inputValue()).toContain('>View file</a>')
    expect(await page.locator('#app .markdown style').textContent()).toContain('<a ')
  }
})

test('Mermaid fences draw once closed, keep their source view while streaming and open a zoomable viewer', async ({ page, browserName }) => {
  const fence = '```mermaid\nflowchart LR\n  A[Start] -->|go| B{Ready?}\n  B --> C((Done))'
  await page.evaluate(text => window.chatRendering.markdown(text), `Plan:\n\n${fence}`)
  await expect(page.locator('#app .markdown pre code')).toContainText('flowchart LR')
  await expect(page.locator('#app figure.diagram')).toHaveCount(0)

  await page.evaluate(text => window.chatRendering.markdown(text), `Plan:\n\n${fence}\n\`\`\`\n\nAfter`)
  const figure = page.locator('#app figure.diagram')
  await expect(figure.locator('svg[role="img"]')).toHaveAttribute('aria-label', 'Flowchart with 3 nodes')
  await expect(figure.locator('svg text')).toContainText(['go', 'Start', 'Ready?', 'Done'])
  await expect(figure.locator('.diagram-source')).toBeHidden()

  await figure.getByRole('button', { name: 'Show source' }).click()
  await expect(figure.getByRole('button', { name: 'Show source' })).toHaveAttribute('aria-pressed', 'true')
  await expect(figure.locator('.diagram-source')).toContainText('A[Start] -->|go| B{Ready?}')
  const drawing = await figure.locator('svg').elementHandle()
  await page.evaluate(text => window.chatRendering.markdown(text), `Plan:\n\n${fence}\n\`\`\`\n\nAfter, continued`)
  expect(await drawing!.evaluate(element => element.isConnected)).toBe(true)
  await expect(figure.locator('.diagram-source')).toBeVisible()

  if (browserName === 'chromium') {
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'])
    await figure.getByRole('button', { name: 'Copy source' }).click()
    await expect(figure.getByRole('button', { name: 'Copy source' })).toHaveAttribute('title', 'Copied')
    expect(await page.evaluate(() => navigator.clipboard.readText())).toContain('flowchart LR')
  }

  await figure.getByRole('button', { name: 'Open diagram' }).click()
  const viewer = page.getByRole('dialog', { name: 'Flowchart' })
  await expect(viewer.locator('svg[role="img"]')).toBeVisible()
  const fitted = await viewer.getByRole('button', { name: 'Fit to view' }).first().textContent()
  await viewer.getByRole('button', { name: 'Zoom in' }).click()
  await expect(viewer.getByRole('button', { name: 'Fit to view' }).first()).not.toHaveText(fitted!)
  await page.keyboard.press('Escape')
  await expect(viewer).toBeHidden()
})

test('unsupported and invalid Mermaid sources stay readable as code', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '```mermaid\ngantt\n  title Plan\n```\n\n```mermaid\nflowchart TD\n  A[Start --> B\n```')
  await expect(page.locator('#app .markdown > pre code')).toContainText('gantt')
  const failed = page.locator('#app figure.diagram-failed')
  await expect(failed.locator('.diagram-error')).toContainText('Couldn’t draw this diagram.')
  await expect(failed.locator('.diagram-source')).toContainText('A[Start --> B')
  await expect(failed.getByRole('button', { name: 'Open diagram' })).toHaveCount(0)
})
