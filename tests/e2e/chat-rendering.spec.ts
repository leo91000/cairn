import type {} from '../../scripts/fixtures/chat-rendering'
import { Buffer } from 'node:buffer'
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

test('raw SVG links retain Markdown link handling and diagram labels retain their text', async ({ page }) => {
  const apiPath = '/apps/web/src/api.ts'
  await page.evaluate(async (path) => {
    const { state } = await import(path)
    state.installationId = 'fixture'
  }, apiPath)
  const source = '<figure class="diagram"><svg width="400" height="100"><a href="https://example.test/docs"><text y="30">Docs</text></a></svg></figure>\n\n<svg width="400" height="100"><a href="/api/runs/r/artifacts/a"><text y="30">/api/runs/r/artifacts/a</text></a></svg>'
  await page.evaluate(text => window.chatRendering.markdown(text, true), source)
  const links = page.locator('#app .markdown svg a')
  await expect(links).toHaveCount(2)
  for (const link of await links.all()) {
    await expect(link).toHaveAttribute('target', '_blank')
    await expect(link).toHaveAttribute('rel', 'noopener noreferrer')
  }

  await expect(links.nth(1)).toHaveAttribute('href', '/api/installations/fixture/api/runs/r/artifacts/a')
  await expect(links.nth(1)).toHaveText('View file')
  await page.evaluate(text => window.chatRendering.markdown(text, true), '\`\`\`mermaid\nflowchart TD\nA[https://example.test/docs]\nclick A "https://example.test/docs"\n\`\`\`')
  await expect(page.locator('#app figure.diagram svg a text')).toHaveText('https://example.test/docs')
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

test('semicolon-separated sequence steps preserve every message and encoded entity', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '```mermaid\nsequenceDiagram;A->>B: first #59; &#59; &amp;; B->>A: response\n```')
  const svg = page.locator('#app figure.diagram svg')
  await expect(svg).toContainText('first ; ; &')
  await expect(svg).toContainText('response')
})

test('group-to-descendant links retain readable source instead of disappearing', async ({ page }) => {
  const source = 'flowchart TD\nsubgraph S\nA\nend\nS-->|start|A'
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
  const failed = page.locator('#app figure.diagram-failed')
  await expect(failed.locator('.diagram-error')).toContainText('Links between a group and its own descendants')
  await expect(failed.locator('.diagram-source')).toHaveText(source)
})

test('identical closed and streaming fences keep their own completion state', async ({ page }) => {
  const source = 'flowchart TD\nA-->B'
  const closed = `\`\`\`mermaid\n${source}\n\`\`\``
  const open = `\`\`\`mermaid\n${source}`
  await page.evaluate(text => window.chatRendering.markdown(text), `${closed}\n\n${open}`)
  await expect(page.locator('#app figure.diagram')).toHaveCount(1)
  await expect(page.locator('#app .markdown > pre code')).toHaveText(`${source}\n`)
  await page.evaluate(text => window.chatRendering.markdown(text), `${closed}\n\n${closed}`)
  await expect(page.locator('#app figure.diagram')).toHaveCount(2)
})

test('node label clicks open their safe link', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart TD\nA[Documentation]\nclick A "https://example.test/docs"\n\`\`\`')
  await page.evaluate(() => {
    document.addEventListener('click', (event) => {
      const anchor = event.target instanceof Element ? event.target.closest('a') : null
      document.body.dataset.diagramLink = anchor?.getAttribute('href') ?? ''
      event.preventDefault()
    }, { once: true })
  })
  // WebKit's protocol bounding box omits SVG text positioning; use its actual DOM geometry.
  const label = await page.locator('#app figure.diagram svg text').evaluate((element) => {
    const {
      x,
      y,
      width,
      height,
    } = element.getBoundingClientRect()
    return {
      x,
      y,
      width,
      height,
    }
  })
  await page.mouse.click(label.x + label.width / 2, label.y + label.height / 2)
  await expect(page.locator('body')).toHaveAttribute('data-diagram-link', 'https://example.test/docs')
})

test('cyclic and unsupported composite semantics preserve readable source', async ({ page }) => {
  const cases = [
    ['flowchart TD\nsubgraph S\nsubgraph S\nA\nend\nend', 'Subgraphs cannot contain themselves.'],
    ['stateDiagram-v2\nstate Active {\nIdle-->Running\n}\nActive --> Active : retry', 'Self-transitions on composite states or subgraphs are not supported.'],
    ['flowchart TD\nsubgraph Active\nIdle-->Running\nend\nActive-->|retry|Active', 'Self-transitions on composite states or subgraphs are not supported.'],
    ['stateDiagram-v2\nstate Active {\n[*]-->A\nA-->[*]\n--\n[*]-->B\nB-->[*]\n}', 'Concurrent state regions are not supported.'],
    ['sequenceDiagram\nparticipant A\ncreate participant B\nA->>B: work', 'Participant creation and destruction are not supported.'],
    ['sequenceDiagram\nparticipant A\ncreate actor B\nA->>B: work', 'Participant creation and destruction are not supported.'],
    ['sequenceDiagram\nparticipant A\ndestroy A\nA->>B: work', 'Participant creation and destruction are not supported.'],
    ['sequenceDiagram\nbox Internal APIs\nparticipant A\nparticipant B\nend\nA->>B: work', 'Sequence participant groups are not supported.'],
  ]
  for (const [source, explanation] of cases) {
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
    await expect(page.locator('#app .diagram-error')).toContainText(explanation!)
    await expect(page.locator('#app .diagram-source')).toHaveText(source!)
    await expect(page.getByRole('button', { name: 'Open diagram' })).toHaveCount(0)
  }
})

test('full-screen artifact nodes retain installation routing and the file viewer', async ({ page, context }) => {
  await page.evaluate(async (path) => {
    const { state } = await import(path)
    state.installationId = 'fixture'
  }, '/apps/web/src/api.ts')
  const path = '/api/installations/fixture/api/runs/run-1/artifacts'
  const first = Promise.withResolvers<void>()
  let lists = 0
  const file = {
    id: 'file-1',
    runId: 'run-1',
    messageId: null,
    key: 'report',
    version: 1,
    title: 'Report',
    name: 'report.txt',
    group: '',
    kind: 'code',
    mediaType: 'text/plain',
    size: 20,
    createdAt: 1,
    url: '/api/runs/run-1/artifacts/file-1',
    previewStatus: 'none',
  }
  await page.route(`**${path}`, async (route) => {
    lists++
    if (lists === 1) {
      await first.promise
      await route.fulfill({ json: [] })
    }
    else {
      await route.fulfill({ json: [file] })
    }
  })
  await page.route(`**${path}/file-1`, route => route.fulfill({ contentType: 'text/plain', body: 'Scoped file contents' }))
  const beforeUrl = page.url()
  const target = `${new URL(beforeUrl).origin}/api/runs/run-1/artifacts/file-1`
  await page.evaluate(source => window.chatRendering.markdown(source), `\`\`\`mermaid\nflowchart TD\nA[Report]\nclick A "${target}"\n\`\`\``)
  await expect(page.locator('#app figure.diagram svg a')).toHaveAttribute('href', `${path}/file-1`)
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const viewer = page.getByRole('dialog', { name: 'Flowchart', exact: true })
  const link = viewer.locator('svg a')
  await expect(link).toHaveAttribute('href', `${path}/file-1`)
  const center = () => link.locator('text').evaluate((element) => {
    const box = element.getBoundingClientRect()
    return { x: box.x + box.width / 2, y: box.y + box.height / 2 }
  })
  let point = await center()
  await page.mouse.move(point.x, point.y)
  await page.mouse.down()
  await page.mouse.move(point.x + 40, point.y + 25, { steps: 5 })
  await page.mouse.up()
  expect(lists).toBe(0)
  const transform = await viewer.locator('.diagram-stage > div').getAttribute('style')
  point = await center()
  await page.mouse.click(point.x, point.y)
  await expect(viewer.getByRole('status')).toHaveText('Opening file…')
  first.resolve()
  await expect(viewer.getByRole('alert')).toHaveText('This file is no longer available.')
  expect(lists).toBe(1)
  point = await center()
  await page.mouse.click(point.x, point.y)
  const artifact = page.getByRole('dialog', { name: 'Deliverables viewer', exact: true })
  await expect(artifact).toBeVisible()
  await expect(artifact).toContainText('Scoped file contents')
  expect(lists).toBe(2)
  expect(context.pages()).toHaveLength(1)
  expect(page.url()).toBe(beforeUrl)
  await artifact.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await expect(viewer).toBeVisible()
  await expect(viewer.locator('.diagram-stage > div')).toHaveAttribute('style', transform!)
})

test('two-pointer gestures preserve the drawing under a moving pinch centroid', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart LR\nA-->B\n\`\`\`')
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const result = await page.locator('.diagram-stage').evaluate(async (element) => {
    const stage = element as HTMLElement
    const drawing = stage.querySelector('div')!
    const frame = () => new Promise(resolve => requestAnimationFrame(resolve))
    await frame()
    const bounds = stage.getBoundingClientRect()
    const cx = bounds.width / 2
    const cy = bounds.height / 2
    const read = () => {
      const matrix = new DOMMatrix(drawing.style.transform)
      return { x: matrix.m41, y: matrix.m42, scale: matrix.m11 }
    }

    const send = (type: string, pointerId: number, x: number, y: number) => stage.dispatchEvent(new PointerEvent(type, {
      pointerId,
      pointerType: 'touch',
      button: 0,
      buttons: type === 'pointerup' ? 0 : 1,
      bubbles: true,
      cancelable: true,
      clientX: bounds.left + x,
      clientY: bounds.top + y,
    }))
    // Synthetic pointer events exercise both engines' handlers; native capture requires a real device pointer.
    const capture = stage.setPointerCapture
    stage.setPointerCapture = () => {}
    try {
      const before = read()
      send('pointerdown', 11, cx - 80, cy)
      send('pointerdown', 12, cx + 80, cy)
      send('pointermove', 11, cx - 80 + 100, cy + 50)
      send('pointermove', 12, cx + 80 + 100, cy + 50)
      await frame()
      const translated = read()
      send('pointermove', 11, cx - 120 + 140, cy + 70)
      send('pointermove', 12, cx + 120 + 140, cy + 70)
      await frame()
      const zoomed = read()
      send('pointerup', 11, cx - 120 + 140, cy + 70)
      send('pointerup', 12, cx + 120 + 140, cy + 70)
      return {
        dx: translated.x - before.x,
        dy: translated.y - before.y,
        translationScale: translated.scale / before.scale,
        zoomScale: zoomed.scale / before.scale,
        beforePoint: { x: (cx - before.x) / before.scale, y: (cy - before.y) / before.scale },
        afterPoint: { x: (cx + 140 - zoomed.x) / zoomed.scale, y: (cy + 70 - zoomed.y) / zoomed.scale },
      }
    }
    finally {
      stage.setPointerCapture = capture
    }
  })
  expect(result.dx).toBeCloseTo(100, 2)
  expect(result.dy).toBeCloseTo(50, 2)
  expect(result.translationScale).toBeCloseTo(1, 3)
  expect(result.zoomScale).toBeCloseTo(1.5, 3)
  expect(result.afterPoint.x).toBeCloseTo(result.beforePoint.x, 2)
  expect(result.afterPoint.y).toBeCloseTo(result.beforePoint.y, 2)
})

test('full-screen node links remain clickable after zoom and pan without navigating on a drag', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart TD\nA[Documentation]\nclick A "https://example.test/docs"\n\`\`\`')
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const viewer = page.getByRole('dialog', { name: 'Flowchart' })
  const label = viewer.locator('svg a text')
  await expect(label).toBeVisible()
  await page.evaluate(() => {
    document.addEventListener('click', (event) => {
      const anchor = event.target instanceof Element ? event.target.closest('a[href]') : null
      if (anchor) {
        document.body.dataset.viewerLink = anchor.getAttribute('href')!
        event.preventDefault()
      }
    })
  })
  const center = () => label.evaluate((element) => {
    const bounds = element.getBoundingClientRect()
    return { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2 }
  })
  let point = await center()
  await page.mouse.click(point.x, point.y)
  await expect(page.locator('body')).toHaveAttribute('data-viewer-link', 'https://example.test/docs')
  await page.evaluate(() => delete document.body.dataset.viewerLink)
  await viewer.getByRole('button', { name: 'Zoom in' }).click()
  point = await center()
  await page.mouse.move(point.x, point.y)
  await page.mouse.down()
  await page.mouse.move(point.x + 80, point.y + 50, { steps: 5 })
  await page.mouse.up()
  await expect(page.locator('body')).not.toHaveAttribute('data-viewer-link')
  const moved = await center()
  expect(moved.x - point.x).toBeGreaterThan(60)
  await page.mouse.click(moved.x, moved.y)
  await expect(page.locator('body')).toHaveAttribute('data-viewer-link', 'https://example.test/docs')
})

test('RGBA and HSLA author styles retain valid SVG colors', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart LR\nA[Red] --> B[Blue]\nstyle A fill:rgba(255,0,0,0.5),stroke:navy\nstyle B fill:hsla(240,100%,50%,0.5),stroke:red\n\`\`\`')
  const nodes = page.locator('#app figure.diagram svg path.diagram-raised')
  await expect(nodes.nth(0)).toHaveCSS('fill', 'rgba(255, 0, 0, 0.5)')
  await expect(nodes.nth(1)).toHaveCSS('fill', 'rgba(0, 0, 255, 0.5)')
})

test('large diagrams export a bounded readable PNG', async ({ page }) => {
  const source = `flowchart LR\n${Array.from({ length: 100 }, (_, index) => `A${index}[Step ${index}] --> A${index + 1}`).join('\n')}`
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const viewer = page.getByRole('dialog', { name: 'Flowchart' })
  const fits = () => viewer.locator('svg').evaluate((element) => {
    const image = element.getBoundingClientRect()
    const stage = element.closest('.diagram-stage')!.getBoundingClientRect()
    return image.left >= stage.left - 1 && image.right <= stage.right + 1 && image.top >= stage.top - 1 && image.bottom <= stage.bottom + 1
  })
  await expect.poll(fits).toBe(true)
  await viewer.getByRole('button', { name: 'Zoom in' }).click()
  await viewer.getByRole('button', { name: 'Fit to view' }).first().click()
  await expect.poll(fits).toBe(true)
  const downloaded = page.waitForEvent('download')
  await viewer.getByRole('button', { name: 'PNG', exact: true }).click()
  const stream = await (await downloaded).createReadStream()
  const chunks: Buffer[] = []
  for await (const chunk of stream!)
    chunks.push(Buffer.from(chunk))
  const png = Buffer.concat(chunks)
  expect(png.subarray(0, 8)).toEqual(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
  const width = png.readUInt32BE(16)
  const height = png.readUInt32BE(20)
  expect(width).toBeGreaterThan(0)
  expect(height).toBeGreaterThan(0)
  expect(Math.max(width, height)).toBeLessThanOrEqual(8192)
  expect(width * height).toBeLessThanOrEqual(16_010_000)
  const hasDrawing = await page.evaluate(async (data) => {
    const image = new Image()
    image.src = `data:image/png;base64,${data}`
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.width
    canvas.height = image.height
    const context = canvas.getContext('2d')!
    context.drawImage(image, 0, 0)
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data
    for (let index = 4; index < pixels.length; index += 4) {
      if (pixels[index] !== pixels[0] || pixels[index + 1] !== pixels[1] || pixels[index + 2] !== pixels[2] || pixels[index + 3] !== pixels[3])
        return true
    }

    return false
  }, png.toString('base64'))
  expect(hasDrawing).toBe(true)
})

test('inline styling keeps later transitions and compact heads keep their node names', async ({ page }) => {
  const source = 'flowchart LR\nclassDef default fill:red;stroke:navy; A---oB\nstyle A fill:blue; B---xC'
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
  await expect(page.locator('#app .diagram-error')).toHaveCount(0)
  await expect(page.locator('#app figure.diagram svg text')).toContainText(['A', 'B', 'C'])
})

test('centering subgraphs preserves separation from unrelated nodes', async ({ page }) => {
  const graph = 'subgraph S0\nN2\nN6\nend\nsubgraph S2\nN0\nN8\nend\nN1-->N8\nN8-->N6\nN8-->N7\nN7-->N0'
  for (const direction of ['LR', 'RL']) {
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\nflowchart ${direction}\n${graph}\n\`\`\``)
    const separated = await page.locator('#app figure.diagram svg').evaluate((svg) => {
      const labels = [...svg.querySelectorAll('text')]
      const a = labels.find(text => text.textContent === 'N6')!.getBoundingClientRect()
      const b = labels.find(text => text.textContent === 'N7')!.getBoundingClientRect()
      return a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top
    })
    expect(separated).toBe(true)
  }
})

test('links between larger subgraphs preserve their whole-group ordering', async ({ page }) => {
  const source = `flowchart TB\nsubgraph S\n${Array.from({ length: 20 }, (_, index) => `A${index}-->A${index + 1}\n`).join('')}end\nsubgraph T\n${Array.from({ length: 21 }, (_, index) => `B${index}\n`).join('')}end\nS-->|group edge|T`
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
  const ordered = await page.locator('#app figure.diagram svg').evaluate((svg) => {
    const frames = [...svg.querySelectorAll('path')].filter(path => path.style.fill === 'var(--dg-cluster)' && path.style.stroke === 'var(--dg-cluster-line)')
    return frames.length === 2 && frames[0]!.getBoundingClientRect().bottom < frames[1]!.getBoundingClientRect().top
  })
  expect(ordered).toBe(true)
})

test('state notes retain their sides and unsupported local directions retain source', async ({ page }) => {
  for (const direction of ['TB', 'BT', 'LR', 'RL']) {
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\nstateDiagram-v2\ndirection ${direction}\nA-->B\nnote left of A : Left\nnote right of A : Right\n\`\`\``)
    const positioned = await page.locator('#app figure.diagram svg').evaluate((svg) => {
      const labels = [...svg.querySelectorAll('text')]
      const center = (label: string) => {
        const box = labels.find(text => text.textContent === label)!.getBoundingClientRect()
        return { x: box.x + box.width / 2, y: box.y + box.height / 2 }
      }

      const state = center('A')
      const left = center('Left')
      const right = center('Right')
      return left.x < state.x && right.x > state.x && Math.abs(left.y - state.y) < 1 && Math.abs(right.y - state.y) < 1
    })
    expect(positioned).toBe(true)
  }

  await page.evaluate(text => window.chatRendering.markdown(text), '```mermaid\nflowchart TB\nsubgraph S\ndirection LR\nA-->B\nend\n```')
  await expect(page.locator('#app .diagram-error')).toContainText('local direction')
  await expect(page.locator('#app .diagram-source')).toContainText('direction LR')
  await expect(page.locator('#app figure.diagram svg')).toHaveCount(0)
})

test('Unicode wrapping, sequence increments and large totals remain readable', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\nflowchart TD\nA["aa${'😀'.repeat(20)}"]\n\`\`\``)
  const unicode = await page.locator('#app figure.diagram svg text').allTextContents()
  expect(unicode.every(line => !/[\uD800-\uDFFF]/u.test(line))).toBe(true)
  await page.evaluate(text => window.chatRendering.markdown(text), '```mermaid\nsequenceDiagram\nautonumber 0.5 0.25\nA->>B: one\nA->>B: two\n```')
  await expect(page.locator('#app figure.diagram svg text')).toContainText(['0.5', '0.75'])
  await page.evaluate(text => window.chatRendering.markdown(text), '```mermaid\npie\n"A":1000000000000000000\n```')
  await expect(page.locator('#app figure.diagram svg text')).toContainText(['Σ', 'Total: 1,000,000,000,000,000,000'])
  expect(await page.locator('#app figure.diagram svg text').filter({ hasText: /^Σ$/ }).evaluate(el => (el as SVGTextElement).getComputedTextLength())).toBeLessThan(88)
  const quoted = '> ```mermaid\n> flowchart TD\n> A-->B\n> ```'
  await page.evaluate(text => window.chatRendering.markdown(text), quoted)
  await expect(page.locator('#app figure.diagram svg')).toHaveCount(1)
  await page.evaluate(text => window.chatRendering.markdown(text), `${quoted}\n>`)
  await expect(page.locator('#app figure.diagram svg')).toHaveCount(1)
})

test('fence closure follows parsed literals in quotes and lists', async ({ page }) => {
  const root = '```mermaid\nflowchart TD\nA-->B'
  const cases = [
    ...['    ```', '\t```', '> ```'].map(line => ({ open: `${root}\n${line}`, close: '\n```' })),
    { open: '> ```mermaid\n> flowchart TD\n> A-->B\n>     ```', close: '\n> ```\n>' },
    { open: '- Item\n\n  ```mermaid\n  flowchart TD\n  A-->B\n      ```', close: '\n  ```' },
  ]
  for (const { open, close } of cases) {
    await page.evaluate(text => window.chatRendering.markdown(text), open)
    await expect(page.locator('#app figure.diagram')).toHaveCount(0)
    await expect(page.locator('#app pre code')).toContainText('A-->B')
    await page.evaluate(text => window.chatRendering.markdown(text), open + close)
    await expect(page.locator('#app figure.diagram')).toHaveCount(1)
  }
})

test('nested activations stay inside the SVG and clear the next lifeline', async ({ page }) => {
  for (const depth of [20, 80]) {
    const source = `sequenceDiagram\nparticipant A\n${depth === 80 ? 'participant B\n' : ''}${'activate A\n'.repeat(depth)}`
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
    const result = await page.locator('#app figure.diagram svg').evaluate((element) => {
      const svg = element as SVGSVGElement
      const bars = [...svg.querySelectorAll('path')].filter(path => path.style.fill === 'var(--dg-data)')
      const right = Math.max(...bars.map(path => path.getBBox().x + path.getBBox().width + 0.5))
      const peer = [...svg.querySelectorAll('text')].find(text => text.textContent === 'B')
      const next = peer ? peer.getBBox().x + peer.getBBox().width / 2 : Infinity
      return { bars: bars.length, contained: right < svg.viewBox.baseVal.width, separated: right < next }
    })
    expect(result).toEqual({ bars: depth, contained: true, separated: true })
  }
})

test('sequence message labels clear wide badges and nested activations', async ({ page }) => {
  for (const number of [1000.25, 1234567890123]) {
    for (const target of ['A', 'B']) {
      const source = `sequenceDiagram\nautonumber ${number}\nA->>${target}: work`
      await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
      const separated = await page.locator('#app figure.diagram svg').evaluate((svg) => {
        const text = [...svg.querySelectorAll('text')].find(item => item.textContent === 'work')!.getBBox()
        const badge = [...svg.querySelectorAll('path')].find(item => item.style.fill === 'var(--dg-edge)' && item.style.stroke === 'none')!.getBBox()
        return text.x > badge.x + badge.width || text.y + text.height < badge.y
      })
      expect(separated).toBe(true)
    }
  }

  for (const depth of [20, 80]) {
    for (const target of ['A', 'B']) {
      const label = 'abcdefghijabcdefghijabcdefghij'
      const source = `sequenceDiagram\nparticipant A\nparticipant B\n${'activate A\n'.repeat(depth)}A->>${target}: ${label}`
      await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
      const separated = await page.locator('#app figure.diagram svg').evaluate((svg) => {
        const texts = [...svg.querySelectorAll('text')]
        const label = texts.find(item => item.textContent === 'abcdefghijabcdefghijabcdefghij')!.getBBox()
        const peer = texts.find(item => item.textContent === 'B')!.getBBox()
        return label.x + label.width < peer.x + peer.width / 2
      })
      expect(separated).toBe(true)
    }
  }
})

test('PNG export preserves invisible fills and labels while drawing their outline', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'light' })
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart TD\nA[Hidden]\nstyle A fill:none,stroke:red,color:none,stroke-width:2\n\`\`\`')
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const download = page.waitForEvent('download')
  await page.getByRole('dialog', { name: 'Flowchart', exact: true }).getByRole('button', { name: 'PNG', exact: true }).click()
  const file = await download
  const parts = []
  for await (const part of (await file.createReadStream())!)
    parts.push(part)
  const pixels = await page.evaluate(async (data) => {
    const image = new Image()
    image.src = data
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.width
    canvas.height = image.height
    const ctx = canvas.getContext('2d')!
    ctx.drawImage(image, 0, 0)
    const rgba = ctx.getImageData(0, 0, image.width, image.height).data
    let red = 0
    let black = 0
    for (let offset = 0; offset < rgba.length; offset += 4) {
      if (rgba[offset + 3]! > 200 && rgba[offset]! > 200 && rgba[offset + 1]! < 30 && rgba[offset + 2]! < 30)
        red++
      if (rgba[offset + 3]! > 200 && rgba[offset]! < 30 && rgba[offset + 1]! < 30 && rgba[offset + 2]! < 30)
        black++
    }

    return { red, black }
  }, `data:image/png;base64,${Buffer.concat(parts).toString('base64')}`)
  expect(pixels.red).toBeGreaterThan(0)
  expect(pixels.black).toBe(0)
})

test('standalone SVG and PNG exports preserve measured text without app fonts', async ({ page, context }) => {
  const label = `${'l'.repeat(100)} Łódź`
  await page.evaluate(async (text) => {
    await document.fonts.load('500 13px "DM Sans Variable"', text)
    await document.fonts.ready
    await window.chatRendering.markdown(`\`\`\`mermaid\npie\n"${text}":1\n\`\`\``)
  }, label)
  await page.getByRole('button', { name: 'Open diagram' }).click()
  const viewer = page.getByRole('dialog', { name: 'Pie chart', exact: true })
  let failFonts = true
  await page.route(/\.woff2(?:\?|$)/, route => failFonts && route.request().resourceType() === 'fetch'
    ? route.fulfill({ status: 503, body: 'Temporary font failure' })
    : route.continue())
  await viewer.getByRole('button', { name: 'SVG', exact: true }).click()
  await expect(viewer.getByRole('alert')).toHaveText('Couldn’t export this diagram. Please try again.')
  await expect(viewer.getByRole('button', { name: 'SVG', exact: true })).toBeEnabled()
  failFonts = false
  const svgDownload = page.waitForEvent('download')
  await viewer.getByRole('button', { name: 'SVG', exact: true }).click()
  const svgFile = await svgDownload
  const svgParts = []
  for await (const part of (await svgFile.createReadStream())!)
    svgParts.push(part)
  const xml = Buffer.concat(svgParts).toString()
  const standalone = await context.newPage()
  let metrics: {
    x: number
    y: number
    width: number
    height: number
    detail: number
    sceneWidth: number
    ink: number[]
  }
  try {
    await standalone.route(/^https?:\/\//, route => route.abort())
    await standalone.setContent(xml)
    metrics = await standalone.evaluate(async (label) => {
      await document.fonts.ready
      const svg = document.querySelector('svg')!
      const texts = [...svg.querySelectorAll('text')]
      const text = texts.find(item => item.textContent === label)!
      const box = text.getBBox()
      const detail = texts.find(item => item.textContent === '100%' && item.getBBox().x > box.x)!.getBBox()
      return {
        x: box.x,
        y: box.y,
        width: box.width,
        height: box.height,
        detail: detail.x,
        sceneWidth: svg.viewBox.baseVal.width,
        ink: getComputedStyle(text).fill.match(/\d+/g)!.slice(0, 3).map(Number),
      }
    }, label)
    expect(metrics.x + metrics.width).toBeLessThan(metrics.detail)
    expect(metrics.x + metrics.width).toBeLessThan(metrics.sceneWidth)
    expect(xml).toContain('data:font/woff2;base64,')
    expect(await standalone.evaluate(() => [...document.fonts].filter(face => face.family.includes('DM Sans') && face.status === 'loaded').length)).toBeGreaterThan(0)
  }
  finally {
    await standalone.close()
  }

  const pngDownload = page.waitForEvent('download')
  await viewer.getByRole('button', { name: 'PNG', exact: true }).click()
  const pngFile = await pngDownload
  const pngParts = []
  for await (const part of (await pngFile.createReadStream())!)
    pngParts.push(part)
  const pixels = await page.evaluate(async ({ data, metrics }) => {
    const image = new Image()
    image.src = data
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.width
    canvas.height = image.height
    const ctx = canvas.getContext('2d')!
    ctx.drawImage(image, 0, 0)
    const ratio = image.width / metrics.sceneWidth
    const rgba = ctx.getImageData(0, 0, image.width, image.height).data
    let right = -1
    for (let y = Math.max(0, Math.floor(metrics.y * ratio) - 2); y < Math.min(image.height, Math.ceil((metrics.y + metrics.height) * ratio) + 2); y++) {
      for (let x = Math.max(0, Math.floor(metrics.x * ratio)); x < image.width; x++) {
        const offset = (y * image.width + x) * 4
        if (rgba[offset + 3]! > 200 && metrics.ink.every((value, channel) => Math.abs(rgba[offset + channel]! - value) < 12))
          right = Math.max(right, x)
      }
    }

    return { right: right / ratio, detail: metrics.detail, expectedRight: metrics.x + metrics.width }
  }, { data: `data:image/png;base64,${Buffer.concat(pngParts).toString('base64')}`, metrics })
  expect(pixels.right).toBeGreaterThan(pixels.expectedRight - 6)
  expect(pixels.right).toBeLessThan(pixels.detail)
})

test('large two-dimensional scenes remain visible in a card and the full-screen viewer', async ({ page }) => {
  const source = `flowchart LR\n${
    Array.from({ length: 90 }, (_, index) => `N${index}[${'W'.repeat(28)}]-->N${index + 1}`).join('\n')
  }\nN50["${Array.from({ length: 230 }).fill('A descriptive step').join('<br>')}"]`
  await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
  await expect(page.locator('#app figure.diagram svg')).toBeVisible()
  await page.getByRole('button', { name: 'Open diagram' }).click()
  await expect(page.locator('.diagram-stage svg')).toBeVisible()
  await page.getByRole('button', { name: 'Close' }).click()
})

test('pie legends retain zero categories with colors matching their positive arcs', async ({ page }) => {
  for (const showData of [false, true]) {
    const source = `pie${showData ? ' showData' : ''}\n"No jobs":0\n"Completed":1\n"Failed":0\n"Waiting":1`
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
    const result = await page.locator('#app figure.diagram svg').evaluate((svg) => {
      const paths = [...svg.querySelectorAll('path')].filter(item => item.style.fill.startsWith('var(--dg-series-'))
      const wedges = paths.filter(item => item.getBBox().width > 20)
      return {
        texts: [...svg.querySelectorAll('text')].map(item => item.textContent),
        wedges: wedges.map(item => item.style.fill),
        swatches: paths.filter(item => !wedges.includes(item)).map(item => item.style.fill),
      }
    })
    for (const label of ['No jobs', 'Completed', 'Failed', 'Waiting'])
      expect(result.texts).toContain(label)
    expect(result.texts.filter(text => text === (showData ? '0% · 0' : '0%'))).toHaveLength(2)
    expect(result.wedges).toEqual(['var(--dg-series-1)', 'var(--dg-series-3)'])
    expect(result.swatches).toEqual([0, 1, 2, 3].map(index => `var(--dg-series-${index})`))
  }
})

test('state class suffixes style their target without entering transition labels', async ({ page }) => {
  for (const label of ['', ' : proceed']) {
    const source = `stateDiagram-v2\nclassDef critical fill:red\nA --> B:::critical${label}`
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
    const result = await page.locator('#app figure.diagram svg').evaluate((svg) => {
      const texts = [...svg.querySelectorAll('text')].map(item => item.textContent)
      const red = [...svg.querySelectorAll('path')].filter(item => item.style.fill === 'red').length
      return { texts, red }
    })
    expect(result.red).toBe(1)
    expect(result.texts).toContain('B')
    expect(result.texts.some(text => text?.includes('critical'))).toBe(false)
    if (label)
      expect(result.texts).toContain('proceed')
  }
})

test('horizontal cluster titles fit their boxes without hiding neighboring titles', async ({ page }) => {
  for (const direction of ['LR', 'RL']) {
    for (const empty of [false, true]) {
      const source = `flowchart ${direction}\nsubgraph S[Long descriptive group title]\n${empty ? '' : 'A[Go]\n'}end\nsubgraph T[Another group]\n${empty ? '' : 'B[Done]\n'}end\nS --> T`
      await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
      const contained = await page.locator('#app figure.diagram svg').evaluate((svg) => {
        const boxes = [...svg.querySelectorAll('path')].filter(path => path.style.fill === 'var(--dg-cluster)' && path.style.stroke !== 'none')
        return ['Long descriptive group title', 'Another group'].every((label, index) => {
          const text = [...svg.querySelectorAll('text')].find(item => item.textContent === label)!.getBBox()
          const box = boxes[index]!.getBBox()
          return text.x >= box.x && text.x + text.width <= box.x + box.width
        })
      })
      expect(contained).toBe(true)
    }
  }
})

for (const manual of [false, true]) {
  test(`an open viewer updates its geometry after delayed app fonts load (manual=${manual})`, async ({ browser, browserName }) => {
    const page = await browser.newPage({ viewport: { width: 1000, height: 800 } })
    const held: Array<import('@playwright/test').Route> = []
    let released = false
    await page.route('**/*dm-sans*.woff2*', (route) => {
      if (released)
        return route.continue()
      held.push(route)
    })
    try {
      await page.goto(`${address}scripts/fixtures/chat-rendering.html`, { waitUntil: 'domcontentloaded' })
      await page.waitForFunction(() => window.chatRendering)
      await page.evaluate(source => window.chatRendering.markdown(source), `\`\`\`mermaid\npie\n"${'l'.repeat(100)} Łódź":1\n"Other":1\n\`\`\``)
      await page.getByRole('button', { name: 'Open diagram' }).click()
      const viewer = page.locator('.diagram-stage svg')
      await expect(viewer).toBeVisible()
      const before = await viewer.getAttribute('viewBox')
      const stage = page.locator('.diagram-stage')
      const drawing = page.locator('.diagram-stage > div')
      if (manual) {
        await page.getByRole('button', { name: 'Zoom in' }).click()
        const area = await stage.boundingBox()
        await page.mouse.move(area!.x + area!.width / 2, area!.y + area!.height / 2)
        await page.mouse.down()
        await page.mouse.move(area!.x + area!.width / 2 + 50, area!.y + area!.height / 2 + 20)
        await page.mouse.up()
      }

      const transform = await drawing.evaluate(element => element.style.transform)
      expect(held.length).toBeGreaterThan(0)
      released = true
      await Promise.all(held.map(route => route.continue()))
      await page.evaluate(async () => {
        await document.fonts.load('500 14px "DM Sans Variable"', 'l'.repeat(100))
        await document.fonts.ready
        await new Promise(resolve => requestAnimationFrame(resolve))
      })
      const card = page.locator('#app figure.diagram svg')
      if (browserName === 'chromium')
        await expect(card).not.toHaveAttribute('viewBox', before!)
      await expect(viewer).toHaveAttribute('viewBox', (await card.getAttribute('viewBox'))!)
      if (manual) {
        expect(await drawing.evaluate(element => element.style.transform)).toBe(transform)
      }
      else {
        const centered = await stage.evaluate((stage) => {
          const area = stage.getBoundingClientRect()
          const drawing = stage.querySelector('svg')!.getBoundingClientRect()
          return Math.abs(drawing.left + drawing.right - area.left - area.right) < 1 && Math.abs(drawing.top + drawing.bottom - area.top - area.bottom) < 1
        })
        expect(centered).toBe(true)
      }
    }
    finally {
      if (!released) {
        released = true
        await Promise.allSettled(held.map(route => route.continue()))
      }

      await page.close()
    }
  })
}

test('nested sequence backgrounds paint the inner highlight above the outer one', async ({ page }) => {
  await page.evaluate(source => window.chatRendering.markdown(source), '```mermaid\nsequenceDiagram\nparticipant A\nrect rgb(255,0,0)\nrect rgb(0,255,0)\nA->>A: Hi\nend\nend\n```')
  const svg = page.locator('#app figure.diagram svg')
  await expect(svg).toBeVisible()
  const point = await svg.evaluate((svg) => {
    const area = svg.getBoundingClientRect()
    const frames = [...svg.querySelectorAll('path')].filter(path => !!path.style.opacity && path.style.fill !== 'none').map(path => path.getBoundingClientRect()).sort((a, b) => a.top - b.top)
    return { x: Math.round(frames[1]!.left - area.left + 12), y: Math.round(frames[1]!.bottom - area.top - 12) }
  })
  const png = await svg.screenshot({ animations: 'disabled' })
  const color = await page.evaluate(async ({ data, point }) => {
    const image = new Image()
    image.src = `data:image/png;base64,${data}`
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.width
    canvas.height = image.height
    const context = canvas.getContext('2d')!
    context.drawImage(image, 0, 0)
    return [...context.getImageData(point.x, point.y, 1, 1).data]
  }, { data: png.toString('base64'), point })
  expect(color[1]).toBeGreaterThan(color[0]!)
})

test('nested sequence frames enclose short and empty children including unused participants', async ({ page }) => {
  const cases = [
    { source: 'sequenceDiagram\nparticipant A\nloop Outer\nloop Inner\nA->>A: Hi\nend\nend', texts: 7 },
    { source: 'sequenceDiagram\nparticipant A\nloop Outer\nloop Inner\nend\nend', texts: 6 },
    { source: 'sequenceDiagram\nparticipant A\nparticipant B\nparticipant C\nloop Outer\nA->>A: Before\nrect rgb(255,0,0)\nend\nend', texts: 9 },
  ]
  for (const { source, texts } of cases) {
    await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${source}\n\`\`\``)
    const svg = page.locator('#app figure.diagram svg')
    await expect(svg.locator('text')).toHaveCount(texts)
    const frames = await svg.evaluate(svg => [...svg.querySelectorAll('path')].filter(path =>
      path.getAttribute('d')?.trim().endsWith('Z')
      && ((path.style.stroke === 'var(--dg-cluster-line)' && Number.parseFloat(path.style.strokeWidth) === 1.25) || (!!path.style.opacity && path.style.fill !== 'none'))).map((path) => {
      const box = path.getBoundingClientRect()
      return {
        left: box.left,
        right: box.right,
        top: box.top,
        bottom: box.bottom,
      }
    }).sort((a, b) => a.top - b.top))
    expect(frames).toHaveLength(2)
    expect(frames[1]!.left).toBeGreaterThan(frames[0]!.left)
    expect(frames[1]!.right).toBeLessThan(frames[0]!.right)
    expect(frames[1]!.top).toBeGreaterThan(frames[0]!.top)
    expect(frames[1]!.bottom).toBeLessThan(frames[0]!.bottom)
  }
})

test('group frames preserve membership across ranks in every direction', async ({ page }) => {
  const cases = [
    { body: 'subgraph S1\nN2\nN8\nend\nsubgraph S2\nN0\nN9\nend\nN3-->N8\nN3-->N7\nN10-->N0', members: [['N2', 'N8'], ['N0', 'N9']], outside: ['N3', 'N7', 'N10'] },
    { body: 'subgraph S0\nN0\nN8\nend\nsubgraph S1\nN1\nN6\nend\nsubgraph S2\nN3\nend\nS2-->S0\nS0-->N6\nN0-->N3', members: [['N0', 'N8'], ['N1', 'N6'], ['N3']], outside: [] },
  ]
  for (const direction of ['TB', 'BT', 'LR', 'RL']) {
    for (const { body, members, outside } of cases) {
      await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\nflowchart ${direction}\n${body}\n\`\`\``)
      const svg = page.locator('#app figure.diagram svg')
      await expect(svg.locator('text')).toHaveCount(members.flat().length + outside.length + members.length)
      const errors = await svg.evaluate((svg, { members, outside }) => {
        const paths = [...svg.querySelectorAll('path')]
        const groups = paths.filter(path => path.style.fill === 'var(--dg-cluster)' && path.style.stroke === 'var(--dg-cluster-line)').map(path => path.getBoundingClientRect())
        const nodes = paths.filter(path => path.classList.contains('diagram-raised')).map(path => path.getBoundingClientRect())
        const disjoint = (a: DOMRect, b: DOMRect) => a.right <= b.left + 0.01 || b.right <= a.left + 0.01 || a.bottom <= b.top + 0.01 || b.bottom <= a.top + 0.01
        const errors: string[] = []
        if (groups.length !== members.length)
          errors.push('Missing group frames')
        for (const title of [...svg.querySelectorAll('text')].filter(text => /^S\d$/.test(text.textContent ?? ''))) {
          if (nodes.some(node => !disjoint(title.getBoundingClientRect(), node)))
            errors.push('Group title overlaps a node')
        }

        for (const [index, group] of groups.entries()) {
          for (const peer of groups.slice(index + 1)) {
            if (!disjoint(group, peer))
              errors.push('Overlapping peer groups')
          }

          for (const name of [...members.flat(), ...outside]) {
            const label = [...svg.querySelectorAll('text')].find(text => text.textContent === name)!.getBoundingClientRect()
            const x = (label.left + label.right) / 2
            const y = (label.top + label.bottom) / 2
            const node = nodes.find(node => node.left <= x && x <= node.right && node.top <= y && y <= node.bottom)!
            const own = members[index]!.includes(name)
            const correct = own
              ? node.left >= group.left - 0.01 && node.right <= group.right + 0.01 && node.top >= group.top - 0.01 && node.bottom <= group.bottom + 0.01
              : disjoint(node, group)
            if (!correct)
              errors.push(`Wrong membership for ${name}`)
          }
        }

        return errors
      }, { members, outside })
      expect(errors, `${direction}: ${body}`).toEqual([])
    }
  }
})

test('multiline flag labels stay inside their outline in every direction', async ({ page }) => {
  for (const direction of ['TD', 'BT', 'LR', 'RL']) {
    for (const lines of [1, 5, 11]) {
      const label = Array.from({ length: lines }).fill('Hello').join('<br>')
      await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\nflowchart ${direction}\nA>"${label}"]\n\`\`\``)
      await expect(page.locator('#app figure.diagram svg tspan')).toHaveCount(lines)
      const contained = await page.locator('#app figure.diagram svg').evaluate((svg) => {
        const path = svg.querySelector<SVGPathElement>('path.diagram-raised')!
        const outline = path.getBBox()
        const notch = outline.x + outline.height / 3
        const inverse = (svg as SVGSVGElement).getScreenCTM()!.inverse()
        return [...svg.querySelectorAll('tspan')].every((span) => {
          const box = span.getBoundingClientRect()
          const left = new DOMPoint(box.left, box.top).matrixTransform(inverse).x
          return left >= notch + 2
        })
      })
      expect(contained, `${direction} ${lines} lines`).toBe(true)
    }
  }
})

test('graph links and loops meet every supported shape outline in all directions', async ({ page }) => {
  const shapes = ['rect', 'rounded', 'stadium', 'subroutine', 'cylinder', 'circle', 'double-circle', 'diamond', 'hexagon', 'lean-r', 'lean-l', 'trap-b', 'trap-t', 'flag', 'doc', 'start', 'stop', 'fork', 'choice']
  for (const direction of ['TB', 'BT', 'LR', 'RL']) {
    for (const shape of shapes) {
      const node = `A@{shape: ${shape}, label: "Ready?"}`
      const header = shape === 'choice' ? `stateDiagram-v2\ndirection ${direction}\nstate A <<choice>>` : `flowchart ${direction}`
      const graphs = shape === 'choice' ? ['A --> A : retry', 'A --> B', 'B --> A'] : [`${node} -->|retry| A`, `${node} --> B[Peer]`, `B[Peer] --> ${node}`]
      for (const graph of graphs) {
        await page.evaluate(text => window.chatRendering.markdown(text), `\`\`\`mermaid\n${header}\n${graph}\n\`\`\``)
        const attached = await page.locator('#app figure.diagram svg').evaluate((svg) => {
          const paths = [...svg.querySelectorAll('path')]
          const curve = paths.find(path => path.style.stroke === 'var(--dg-edge)' && path.style.fill === 'none')!
          const head = paths.find(path => path.style.fill === 'var(--dg-edge)')!
          const outlines = paths.filter(path => !path.closest('defs') && path.style.stroke && path.style.stroke !== 'none' && path.style.stroke !== 'var(--dg-edge)')
          return [curve.getPointAtLength(0), head.getPointAtLength(0)].every(point => outlines.some(path => path.isPointInStroke(new DOMPoint(point.x, point.y))))
        })
        expect(attached, `${direction} ${shape}: ${graph}`).toBe(true)
      }
    }
  }
})
