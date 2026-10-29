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

test('cyclic subgraphs fall back to readable source without blocking the page', async ({ page }) => {
  await page.evaluate(text => window.chatRendering.markdown(text), '\`\`\`mermaid\nflowchart TD\nsubgraph S\nsubgraph S\nA\nend\nend\n\`\`\`')
  await expect(page.locator('#app .diagram-error')).toContainText('Subgraphs cannot contain themselves.')
  await expect(page.locator('#app .diagram-source')).toContainText('subgraph S')
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
