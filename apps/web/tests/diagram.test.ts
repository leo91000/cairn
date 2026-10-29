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
