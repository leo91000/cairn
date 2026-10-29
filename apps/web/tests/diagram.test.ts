import type { PathItem, Scene } from '../src/diagram-layout'
import { describe, expect, it } from 'vitest'
import corpus from '../../../tests/fixtures/diagrams.json'
import {
  cleanLabel,
  DiagramError,
  diagramKind,
  isDiagramLanguage,
  parseDiagram,
} from '../src/diagram'
import { layoutDiagram, lineHeight, wrapText } from '../src/diagram-layout'

// Android's DiagramCorpusTest uses the same measure, so both layouts must agree exactly.
const measure = { width: (text: string, size: number) => text.length * size * 0.55 }

function points(item: PathItem) {
  return item.d.flatMap(([op, ...values]) => {
    if (op === 'Z')
      return []
    const pairs: [number, number][] = []
    for (let index = 0; index < values.length; index += 2)
      pairs.push([values[index]!, values[index + 1]!])
    return pairs
  })
}

function nodeBoxes(scene: Scene) {
  return scene.items
    .filter((item): item is PathItem => item.kind === 'path' && !!item.shadow)
    .map((item) => {
      const xs = points(item).map(([x]) => x)
      const ys = points(item).map(([, y]) => y)
      return {
        left: Math.min(...xs),
        right: Math.max(...xs),
        top: Math.min(...ys),
        bottom: Math.max(...ys),
      }
    })
}

describe('diagrams (shared Android corpus)', () => {
  it.each(corpus.parse)('parses $name like Android', ({ source, diagram }) => {
    expect(parseDiagram(source)).toEqual(diagram)
  })

  it.each(corpus.parse)('lays out $name like Android', ({ source, layout }) => {
    const scene = layoutDiagram(parseDiagram(source), measure)
    expect({ width: scene.width, height: scene.height, items: scene.items.length }).toEqual(layout)
  })

  it.each(corpus.errors)('rejects $name', ({ source }) => {
    expect(() => layoutDiagram(parseDiagram(source), measure)).toThrow(DiagramError)
  })

  it.each(corpus.unsupported)('keeps unsupported source %#', (source) => {
    expect(diagramKind(source)).toBeUndefined()
    expect(() => parseDiagram(source)).toThrow(DiagramError)
  })
})

describe('diagram layout', () => {
  it.each(corpus.parse)('keeps $name inside its bounds without overlapping nodes', ({ source }) => {
    const scene = layoutDiagram(parseDiagram(source), measure)
    for (const item of scene.items) {
      if (item.kind === 'text') {
        const width = Math.max(0, ...item.lines.map(line => measure.width(line, item.size)))
        const left = item.x - (item.anchor === 'middle' ? width / 2 : item.anchor === 'end' ? width : 0)
        const height = item.lines.length * lineHeight(item.size)
        expect(left).toBeGreaterThanOrEqual(-1)
        expect(left + width).toBeLessThanOrEqual(scene.width + 1)
        expect(item.y - height / 2).toBeGreaterThanOrEqual(-1)
        expect(item.y + height / 2).toBeLessThanOrEqual(scene.height + 1)
        continue
      }

      for (const [x, y] of points(item)) {
        expect(x).toBeGreaterThanOrEqual(-1)
        expect(y).toBeGreaterThanOrEqual(-1)
        expect(x).toBeLessThanOrEqual(scene.width + 1)
        expect(y).toBeLessThanOrEqual(scene.height + 1)
      }
    }

    const boxes = nodeBoxes(scene)
    for (const [index, a] of boxes.entries()) {
      for (const b of boxes.slice(index + 1)) {
        const separate = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top
        expect(separate).toBe(true)
      }
    }
  })

  it.each(['TB', 'BT', 'LR', 'RL'])('keeps state notes on their physical sides in %s', (direction) => {
    const scene = layoutDiagram(parseDiagram(`stateDiagram-v2\ndirection ${direction}\nA-->B\nnote left of A : Left\nnote right of A : Right`), measure)
    const texts = scene.items.filter(item => item.kind === 'text')
    const state = texts.find(item => item.lines[0] === 'A')!
    const left = texts.find(item => item.lines[0] === 'Left')!
    const right = texts.find(item => item.lines[0] === 'Right')!
    expect(left.x).toBeLessThan(state.x)
    expect(right.x).toBeGreaterThan(state.x)
    expect(left.y).toBeCloseTo(state.y)
    expect(right.y).toBeCloseTo(state.y)
  })

  it('wraps supplementary characters without splitting surrogate pairs', () => {
    const word = `aa${'😀'.repeat(20)}`
    const lines = wrapText(word, 14, 500, 200, measure)
    expect(lines.join('')).toBe(word)
    expect(lines.every(line => !/[\uD800-\uDFFF]/u.test(line))).toBe(true)
  })

  it('uses configured sequence increments and exact hundredths', () => {
    const diagram = parseDiagram('sequenceDiagram\nautonumber 0.5 0.25\nA->>B: one\nA->>B: two\nA->>B: three')
    expect(diagram.kind).toBe('sequence')
    if (diagram.kind === 'sequence')
      expect(diagram.items.filter(item => item.type === 'message').map(item => item.number)).toEqual([0.5, 0.75, 1])
  })

  it('moves large pie totals to the legend without clipping their digits', () => {
    const scene = layoutDiagram(parseDiagram('pie\n"A":1000000000000000000'), measure)
    const texts = scene.items.filter(item => item.kind === 'text')
    const center = texts.find(item => item.size === 18)!
    expect(measure.width(center.lines[0]!, center.size)).toBeLessThan(88)
    expect(texts.flatMap(item => item.lines)).toContain('Total: 1,000,000,000,000,000,000')
  })

  it('separates document endpoints from a composite named root', () => {
    const graph = parseDiagram('stateDiagram-v2\n[*] --> root\nstate root {\n[*] --> Ready\nReady --> [*]\n}\nroot --> [*]')
    expect(graph.kind).toBe('graph')
    if (graph.kind !== 'graph')
      return
    const endpoints = graph.nodes.filter(node => node.shape === 'start' || node.shape === 'end')
    expect(endpoints).toHaveLength(4)
    expect(endpoints.filter(node => node.parent === undefined)).toHaveLength(2)
    expect(endpoints.filter(node => node.parent === 'root')).toHaveLength(2)
  })

  it('draws every flowchart node, link and label', () => {
    const scene = layoutDiagram(parseDiagram('flowchart LR\n  A[One] -->|go| B{Two?}\n  B --> C((Three))'), measure)
    const texts = scene.items.flatMap(item => item.kind === 'text' ? item.lines : [])
    expect(texts).toEqual(['go', 'One', 'Two?', 'Three'])
    expect(scene.label).toBe('Flowchart with 3 nodes')
  })

  it('keeps author colors readable and links safe', () => {
    const diagram = parseDiagram('flowchart TD\n  A --> B\n  style A fill:#14365a\n  style B fill:url(#x),color:expression(alert(1))\n  click A "javascript:alert(1)"\n  click B "https://example.com"')
    expect(diagram.kind === 'graph' && diagram.nodes.map(node => [node.style, node.link])).toEqual([
      [{ fill: '#14365a' }, undefined],
      [undefined, 'https://example.com/'],
    ])
    const scene = layoutDiagram(diagram, measure)
    const label = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'A')
    expect(label?.kind === 'text' && label.fill).toEqual({ color: '#ffffff' })
  })

  it.each(corpus.colors)('keeps text readable on $fill in either theme', ({ fill, ink }) => {
    const scene = layoutDiagram(parseDiagram(`flowchart TD\nA[Readable label]\nstyle A fill:${fill}`), measure)
    const label = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'Readable label')
    expect(label?.kind === 'text' && label.fill).toEqual({ color: ink })
  })

  it('preserves both self-loop arrowheads and author dash styles', () => {
    const one = layoutDiagram(parseDiagram('flowchart TD\nA --> A'), measure)
    const both = layoutDiagram(parseDiagram('flowchart TD\nA <--> A\nlinkStyle 0 stroke-dasharray:4 2'), measure)
    expect(both.items.length).toBe(one.items.length + 1)
    const loop = both.items.find(item => item.kind === 'path' && item.d.some(([op]) => op === 'C') && item.stroke && 'tone' in item.stroke && item.stroke.tone === 'edge')
    expect(loop?.kind === 'path' && loop.dash).toEqual([4, 4])
  })

  it.each(['TD', 'BT', 'LR', 'RL'])('separates multiple self-loop routes and labels in %s', (direction) => {
    const scene = layoutDiagram(parseDiagram(`flowchart ${direction}\nA -->|retry| A\nA -->|cancel<br>and stop| A`), measure)
    const routes = scene.items.filter(item => item.kind === 'path' && !item.fill && item.stroke && 'tone' in item.stroke && item.stroke.tone === 'edge' && item.d.some(([op]) => op === 'C'))
    expect(new Set(routes.map(item => JSON.stringify(item))).size).toBe(2)
    const labels = scene.items.filter(item => item.kind === 'text' && ['retry', 'cancel'].includes(item.lines[0]!))
    const boxes = labels.map((item) => {
      if (item.kind !== 'text')
        throw new Error('Expected a loop label')
      const halfWidth = Math.max(...item.lines.map(line => measure.width(line, item.size))) / 2
      const halfHeight = item.lines.length * lineHeight(item.size) / 2
      return {
        left: item.x - halfWidth,
        right: item.x + halfWidth,
        top: item.y - halfHeight,
        bottom: item.y + halfHeight,
      }
    })
    expect(boxes).toHaveLength(2)
    const [a, b] = boxes
    expect(a!.right <= b!.left || b!.right <= a!.left || a!.bottom <= b!.top || b!.bottom <= a!.top).toBe(true)
  })

  it.each(['circle', 'doublecircle'])('keeps multiline text inside the %s outline', (shape) => {
    const label = Array.from({ length: 10 }).fill('abcdefghijklmnopqrstuvwxy').join('<br>')
    const source = shape === 'circle' ? `flowchart TD\nA(("${label}"))` : `flowchart TD\nA((("${label}")))`
    const scene = layoutDiagram(parseDiagram(source), measure)
    const box = nodeBoxes(scene)[0]!
    const text = scene.items.find(item => item.kind === 'text' && item.lines.length === 10)!
    if (text.kind !== 'text')
      throw new Error('Expected a multiline label')
    const width = Math.max(...text.lines.map(line => measure.width(line, text.size)))
    const height = text.lines.length * lineHeight(text.size)
    const innerRadius = (box.right - box.left) / 2 - (shape === 'doublecircle' ? 5 : 0)
    expect(Math.hypot(width / 2, height / 2)).toBeLessThan(innerRadius)
  })

  it('parses compact circle/cross heads without changing target IDs', () => {
    const graph = parseDiagram('flowchart LR\nA---oB\nB---xC')
    expect(graph.kind === 'graph' && graph.nodes.map(node => node.id)).toEqual(['A', 'B', 'C'])
    expect(graph.kind === 'graph' && graph.edges.map(edge => edge.end)).toEqual(['circle', 'cross'])
  })

  it('retains graph statements after semicolon-separated styling properties', () => {
    const graph = parseDiagram('flowchart TD\nclassDef default fill:red;stroke:navy; A-->B\nstyle A fill:blue; B-->C')
    expect(graph.kind === 'graph' && graph.nodes.map(node => node.id)).toEqual(['A', 'B', 'C'])
    expect(graph.kind === 'graph' && graph.nodes[0]!.style).toEqual({ fill: 'blue', stroke: 'navy' })
    expect(graph.kind === 'graph' && graph.edges.map(edge => [edge.from, edge.to])).toEqual([['A', 'B'], ['B', 'C']])
  })

  it('orders whole subgraphs beyond 400 member constraints', () => {
    const source = corpus.parse.find(item => item.name === 'full-cluster-constraints')!.source
    const scene = layoutDiagram(parseDiagram(source), measure)
    const clusters = scene.items.filter((item): item is PathItem => item.kind === 'path' && !!item.fill && 'tone' in item.fill && item.fill.tone === 'cluster' && !!item.stroke)
    expect(clusters).toHaveLength(2)
    const sourceBottom = Math.max(...points(clusters[0]!).map(([, y]) => y))
    const targetTop = Math.min(...points(clusters[1]!).map(([, y]) => y))
    expect(sourceBottom).toBeLessThan(targetTop)
  })

  it('wraps long labels between words and inside long words', () => {
    expect(wrapText('alpha beta gamma', 10, 400, 50, measure)).toEqual(['alpha', 'beta', 'gamma'])
    expect(wrapText('abcdefghijklmnop', 10, 400, 30, measure)).toEqual(['abcde', 'fghij', 'klmno', 'p'])
    expect(wrapText('one\ntwo', 10, 400, 200, measure)).toEqual(['one', 'two'])
  })

  it('wraps an adversarial long word with bounded measurement work', () => {
    const source = 'a'.repeat(15_000)
    let calls = 0
    let characters = 0
    const lines = wrapText(source, 14, 600, 200, {
      width(text, size) {
        calls++
        characters += text.length
        return measure.width(text, size)
      },
    })
    expect(lines.join('')).toBe(source)
    expect(lines.every(line => measure.width(line, 14) <= 200)).toBe(true)
    expect(calls).toBeLessThan(source.length)
    expect(characters).toBeLessThan(source.length * 30)
  })

  it('cleans Mermaid label markup', () => {
    expect(cleanLabel('"A<br/>B #amp; <b>C</b>"')).toBe('A\nB & C')
    expect(isDiagramLanguage('mermaid')).toBe(true)
    expect(isDiagramLanguage('js')).toBe(false)
  })
})
