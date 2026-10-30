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
  it('paints nested sequence backgrounds from parent to child', () => {
    const scene = layoutDiagram(parseDiagram('sequenceDiagram\nparticipant A\nrect rgb(255,0,0)\nrect rgb(0,255,0)\nA->>A: Hi\nend\nend'), measure)
    const colors = scene.items.filter((item): item is PathItem => item.kind === 'path' && item.opacity !== undefined && !!item.fill && 'color' in item.fill).map(item => item.fill && 'color' in item.fill ? item.fill.color : undefined)
    expect(colors).toEqual(['rgb(255,0,0)', 'rgb(0,255,0)'])
  })

  it.each([
    'sequenceDiagram\nparticipant A\nloop Outer\nloop Inner\nA->>A: Hi\nend\nend',
    'sequenceDiagram\nparticipant A\nloop Outer\nloop Inner\nend\nend',
    'sequenceDiagram\nparticipant A\nparticipant B\nparticipant C\nloop Outer\nA->>A: Before\nrect rgb(255,0,0)\nend\nend',
  ])('keeps complete nested sequence frames within their parents %#', (source) => {
    const scene = layoutDiagram(parseDiagram(source), measure)
    const frames = scene.items.filter((item): item is PathItem => item.kind === 'path' && item.d.at(-1)?.[0] === 'Z' && ((item.stroke && 'tone' in item.stroke && item.stroke.tone === 'cluster-line' && item.width === 1.25) || (item.opacity !== undefined && !!item.fill))).map((item) => {
      const vertices = points(item)
      return {
        left: Math.min(...vertices.map(([x]) => x)),
        right: Math.max(...vertices.map(([x]) => x)),
        top: Math.min(...vertices.map(([, y]) => y)),
        bottom: Math.max(...vertices.map(([, y]) => y)),
      }
    }).sort((a, b) => a.top - b.top)
    expect(frames).toHaveLength(2)
    const [outer, inner] = frames as [typeof frames[number], typeof frames[number]]
    expect(inner.left).toBeGreaterThanOrEqual(outer.left + 4)
    expect(inner.right).toBeLessThanOrEqual(outer.right - 4)
    expect(inner.top).toBeGreaterThanOrEqual(outer.top + 4)
    expect(inner.bottom).toBeLessThanOrEqual(outer.bottom - 4)
  })

  it.each(['TB', 'BT', 'LR', 'RL'])('keeps group titles away from nodes in %s', (direction) => {
    const scene = layoutDiagram(parseDiagram(`flowchart ${direction}\nsubgraph S[Group]\nA\nB\nend\nA-->B`), measure)
    const title = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'Group')!
    if (title.kind !== 'text')
      throw new Error('Expected a group title')
    const right = title.x + measure.width('Group', title.size)
    const top = title.y - lineHeight(title.size) / 2
    const bottom = title.y + lineHeight(title.size) / 2
    for (const node of nodeBoxes(scene))
      expect(right <= node.left || node.right <= title.x || bottom <= node.top || node.bottom <= top).toBe(true)
  })

  it.each(['TB', 'BT', 'LR', 'RL'].flatMap(direction => [
    'subgraph S1\nN2\nN8\nend\nsubgraph S2\nN0\nN9\nend\nN3-->N8\nN3-->N7\nN10-->N0',
    'subgraph S0\nN0\nN8\nend\nsubgraph S1\nN1\nN6\nend\nsubgraph S2\nN3\nend\nS2-->S0\nS0-->N6\nN0-->N3',
  ].map(body => ({ direction, body }))))('preserves group membership across ranks in $direction %#', ({ direction, body }) => {
    const graph = parseDiagram(`flowchart ${direction}\n${body}`)
    if (graph.kind !== 'graph')
      throw new Error('Expected a graph')
    const scene = layoutDiagram(graph, measure)
    const groups = scene.items.filter((item): item is PathItem => item.kind === 'path' && !!item.fill && 'tone' in item.fill && item.fill.tone === 'cluster' && !!item.stroke).map((item) => {
      const vertices = points(item)
      return {
        left: Math.min(...vertices.map(([x]) => x)),
        right: Math.max(...vertices.map(([x]) => x)),
        top: Math.min(...vertices.map(([, y]) => y)),
        bottom: Math.max(...vertices.map(([, y]) => y)),
      }
    })
    const disjoint = (a: typeof groups[number], b: typeof groups[number]) => a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top
    expect(groups).toHaveLength(graph.clusters.length)
    for (const [index, group] of groups.entries()) {
      for (const peer of groups.slice(index + 1))
        expect(disjoint(group, peer)).toBe(true)
      for (const node of graph.nodes) {
        const label = scene.items.find(item => item.kind === 'text' && item.lines[0] === node.label)!
        if (label.kind !== 'text')
          throw new Error('Expected a node label')
        const box = nodeBoxes(scene).find(box => box.left <= label.x && label.x <= box.right && box.top <= label.y && label.y <= box.bottom)!
        expect(box).toBeDefined()
        if (node.parent === graph.clusters[index]!.id) {
          expect(box.left).toBeGreaterThanOrEqual(group.left)
          expect(box.right).toBeLessThanOrEqual(group.right)
          expect(box.top).toBeGreaterThanOrEqual(group.top)
          expect(box.bottom).toBeLessThanOrEqual(group.bottom)
        }
        else {
          expect(disjoint(box, group), `${node.id} must stay outside ${graph.clusters[index]!.id}`).toBe(true)
        }
      }
    }
  })

  it.each([
    ['sequenceDiagram; A->>B: first\nB->>A: response', 'sequenceDiagram\nA->>B: first\nB->>A: response'],
    ['sequenceDiagram;participant A as Alice;participant B as Bob;autonumber;A->>B: First #59; &#59; &amp;;B-->>A: Response', 'sequenceDiagram\nparticipant A as Alice\nparticipant B as Bob\nautonumber\nA->>B: First #59; &#59; &amp;\nB-->>A: Response'],
  ])('preserves semicolon-separated sequence steps %#', (compact, expanded) => {
    expect(parseDiagram(compact)).toEqual(parseDiagram(expanded))
  })

  it.each([
    'flowchart TD\nsubgraph S\nA\nend\nS-->|start|A',
    'flowchart TD\nsubgraph S\nA\nend\nA-->|return|S',
    'flowchart LR\nsubgraph S\nsubgraph T\nA\nend\nend\nS-->T',
    'stateDiagram-v2\nstate S {\nA\n}\nS-->A: enter',
  ])('keeps unsupported group-to-descendant links as readable source %#', (source) => {
    expect(() => parseDiagram(source)).toThrow(DiagramError)
  })

  it.each(['TD', 'BT', 'LR', 'RL'].flatMap(direction => [1, 5, 11].map(lines => ({ direction, lines }))))('keeps $lines-line flag labels beyond the notch in $direction', ({ direction, lines }) => {
    const scene = layoutDiagram(parseDiagram(`flowchart ${direction}\nA>"${Array.from({ length: lines }).fill('Hello').join('<br>')}"]`), measure)
    const outline = scene.items.find((item): item is PathItem => item.kind === 'path' && !!item.shadow)!
    const text = scene.items.find(item => item.kind === 'text')!
    if (text.kind !== 'text')
      throw new Error('Expected a flag label')
    const notch = points(outline)[4]![0]
    const left = text.x - Math.max(...text.lines.map(line => measure.width(line, text.size))) / 2
    expect(left - notch).toBeGreaterThanOrEqual(8)
  })

  it.each([
    'stateDiagram-v2\nstate Active {\nIdle-->Running\n}\nActive --> Active : retry',
    'flowchart TD\nsubgraph Active\nIdle-->Running\nend\nActive-->|retry|Active',
    'stateDiagram-v2\nstate Active {\n[*]-->A\nA-->[*]\n--\n[*]-->B\nB-->[*]\n}',
  ])('keeps unsupported composite semantics as readable source %#', (source) => {
    expect(() => parseDiagram(source)).toThrow(DiagramError)
  })

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

  it.each([false, true])('keeps zero categories and matching colors in pie legends (showData=%s)', (showData) => {
    const source = `pie${showData ? ' showData' : ''}\n"No jobs":0\n"Completed":1\n"Failed":0\n"Waiting":1`
    const scene = layoutDiagram(parseDiagram(source), measure)
    const texts = scene.items.filter(item => item.kind === 'text').flatMap(item => item.lines)
    for (const label of ['No jobs', 'Completed', 'Failed', 'Waiting'])
      expect(texts).toContain(label)
    expect(texts.filter(text => text === (showData ? '0% · 0' : '0%'))).toHaveLength(2)
    const series = scene.items.filter((item): item is PathItem => item.kind === 'path' && !!item.fill && 'tone' in item.fill && item.fill.tone.startsWith('series-'))
    const wedges = series.filter(item => Math.max(...points(item).map(([x]) => x)) - Math.min(...points(item).map(([x]) => x)) > 20)
    expect(wedges.map(item => item.fill)).toEqual([{ tone: 'series-1' }, { tone: 'series-3' }])
    expect(series.filter(item => !wedges.includes(item)).map(item => item.fill)).toEqual([0, 1, 2, 3].map(index => ({ tone: `series-${index}` })))
  })

  it('moves large pie totals to the legend without clipping their digits', () => {
    const scene = layoutDiagram(parseDiagram('pie\n"A":1000000000000000000'), measure)
    const texts = scene.items.filter(item => item.kind === 'text')
    const center = texts.find(item => item.size === 18)!
    expect(measure.width(center.lines[0]!, center.size)).toBeLessThan(88)
    expect(texts.flatMap(item => item.lines)).toContain('Total: 1,000,000,000,000,000,000')
  })

  it.each([
    { statement: 'A --> B:::critical', labels: [''] },
    { statement: 'A --> B:::critical : proceed', labels: ['proceed'] },
    { statement: 'A:::critical --> B:::critical : go', labels: ['go'] },
    { statement: 'B:::critical : Ready', labels: [] },
  ])('preserves state class suffixes in $statement', ({ statement, labels }) => {
    const graph = parseDiagram(`stateDiagram-v2\nclassDef critical fill:red\n${statement}`)
    if (graph.kind !== 'graph')
      throw new Error('Expected a graph')
    expect(graph.nodes.find(node => node.id === 'B')!.style?.fill).toBe('red')
    expect(graph.edges.map(edge => edge.label)).toEqual(labels)
    if (!labels.length)
      expect(graph.nodes.find(node => node.id === 'B')!.label).toBe('Ready')
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

  it.each(['box Internal APIs', 'box rgba(100,100,100,0.1) Internal APIs'])('retains unsupported participant group %s as source', (group) => {
    expect(() => parseDiagram(`sequenceDiagram\n${group}\nparticipant A\nparticipant B\nend\nA->>B: work`)).toThrow(DiagramError)
  })

  it.each(['create participant B', 'create actor B', 'destroy A'])('retains unsupported lifecycle directive %s as source', (directive) => {
    expect(() => parseDiagram(`sequenceDiagram\nparticipant A\n${directive}\nA->>B: work`)).toThrow(DiagramError)
  })

  it.each([1000.25, 1234567890123])('keeps self-message text clear of badge %s', (number) => {
    const scene = layoutDiagram(parseDiagram(`sequenceDiagram\nautonumber ${number}\nA->>A: work`), measure)
    const text = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'work')!
    const badge = scene.items.find((item): item is PathItem => item.kind === 'path' && !item.stroke && !!item.fill && 'tone' in item.fill && item.fill.tone === 'edge')!
    expect(text.kind === 'text' && text.x).toBeGreaterThan(Math.max(...points(badge).map(([x]) => x)))
  })

  it.each([20, 80].flatMap(depth => ['A', 'B'].map(target => ({ depth, target }))))('keeps activated $target message labels clear at depth $depth', ({ depth, target }) => {
    const label = 'abcdefghijabcdefghijabcdefghij'
    const source = `sequenceDiagram\nparticipant A\nparticipant B\n${'activate A\n'.repeat(depth)}A->>${target}: ${label}`
    const scene = layoutDiagram(parseDiagram(source), measure)
    const text = scene.items.find(item => item.kind === 'text' && item.lines[0] === label)!
    const peer = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'B')!
    if (text.kind !== 'text' || peer.kind !== 'text')
      throw new Error('Expected a message and peer')
    const right = text.x + measure.width(label, text.size) * (text.anchor === 'middle' ? 0.5 : 1)
    expect(right).toBeLessThan(peer.x)
  })

  it('fits deep activations and leaves neighboring lifelines and frames clear', () => {
    const sources = [
      `sequenceDiagram\nparticipant A\n${'activate A\n'.repeat(20)}`,
      `sequenceDiagram\nparticipant A\nparticipant B\n${'activate A\n'.repeat(80)}`,
      `sequenceDiagram\nparticipant A\nloop Nested\n${'activate A\n'.repeat(80)}end`,
    ]
    for (const source of sources) {
      const scene = layoutDiagram(parseDiagram(source), measure)
      const bars = scene.items.filter((item): item is PathItem => item.kind === 'path' && !!item.fill && 'tone' in item.fill && item.fill.tone === 'data')
      const right = Math.max(...bars.flatMap(item => points(item).map(([x]) => x + (item.width ?? 0) / 2)))
      expect(right).toBeLessThan(scene.width)
      const peer = scene.items.find(item => item.kind === 'text' && item.lines[0] === 'B')
      if (peer?.kind === 'text')
        expect(right).toBeLessThan(peer.x)
      const frame = scene.items.find((item): item is PathItem => item.kind === 'path' && !!item.stroke && 'tone' in item.stroke && item.stroke.tone === 'cluster-line' && !item.dash)
      if (frame)
        expect(right).toBeLessThan(Math.max(...points(frame).map(([x]) => x)))
    }
  })

  it.each(['TB', 'BT', 'LR', 'RL'].flatMap(direction => ['diamond', 'circle', 'choice'].map(shape => ({ direction, shape }))))('attaches $shape loops to their outline in $direction', ({ direction, shape }) => {
    const node = shape === 'diamond' ? 'A{Ready?}' : 'A((Ready?))'
    const source = shape === 'choice' ? `stateDiagram-v2\ndirection ${direction}\nstate A <<choice>>\nA --> A : retry` : `flowchart ${direction}\n${node} -->|retry| A`
    const scene = layoutDiagram(parseDiagram(source), measure)
    const outline = scene.items.find((item): item is PathItem => item.kind === 'path' && !!item.shadow)!
    const vertices = points(outline)
    const left = Math.min(...vertices.map(([x]) => x))
    const right = Math.max(...vertices.map(([x]) => x))
    const top = Math.min(...vertices.map(([, y]) => y))
    const bottom = Math.max(...vertices.map(([, y]) => y))
    const curve = scene.items.find((item): item is PathItem => item.kind === 'path' && !item.fill && !!item.stroke && 'tone' in item.stroke && item.stroke.tone === 'edge')!
    const head = scene.items.find((item): item is PathItem => item.kind === 'path' && !!item.fill && 'tone' in item.fill && item.fill.tone === 'edge')!
    for (const [x, y] of [points(curve)[0]!, points(head)[0]!]) {
      const dx = (x - (left + right) / 2) / ((right - left) / 2)
      const dy = (y - (top + bottom) / 2) / ((bottom - top) / 2)
      const boundary = shape !== 'circle' ? Math.abs(dx) + Math.abs(dy) : dx * dx + dy * dy
      expect(boundary).toBeCloseTo(1, shape !== 'circle' ? 6 : 2)
    }
  })

  it('attaches horizontal links to a slanted node edge', () => {
    const scene = layoutDiagram(parseDiagram('flowchart LR\nA[/Ready?/] --> B[Done]'), measure)
    const node = scene.items.find((item): item is PathItem => item.kind === 'path' && !!item.shadow)!
    const vertices = points(node)
    const edge = scene.items.find((item): item is PathItem => item.kind === 'path' && !item.fill && !!item.stroke && 'tone' in item.stroke && item.stroke.tone === 'edge')!
    expect(points(edge)[0]![0]).toBeCloseTo((vertices[1]![0] + vertices[2]![0]) / 2, 6)
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

  it.each(['LR', 'RL'].flatMap(direction => [false, true].map(empty => ({ direction, empty }))))('keeps horizontal cluster titles inside their boxes in $direction (empty=$empty)', ({ direction, empty }) => {
    const labels = ['Long descriptive group title', 'Another group']
    const source = `flowchart ${direction}\nsubgraph S[${labels[0]}]\n${empty ? '' : 'A[Go]\n'}end\nsubgraph T[${labels[1]}]\n${empty ? '' : 'B[Done]\n'}end\nS --> T`
    const scene = layoutDiagram(parseDiagram(source), measure)
    const boxes = scene.items.filter((item): item is PathItem => item.kind === 'path' && !!item.stroke && !!item.fill && 'tone' in item.fill && item.fill.tone === 'cluster')
    expect(boxes).toHaveLength(2)
    for (const [index, label] of labels.entries()) {
      const text = scene.items.find(item => item.kind === 'text' && item.lines[0] === label)!
      if (text.kind !== 'text')
        throw new Error('Expected a cluster title')
      const right = Math.max(...points(boxes[index]!).map(([x]) => x))
      expect(text.x + measure.width(label, text.size)).toBeLessThanOrEqual(right)
    }
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
