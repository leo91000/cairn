import type {
  Diagram,
  DiagramStyle,
  EdgeHead,
  GraphDiagram,
  GraphEdge,
  GraphNode,
  NodeShape,
  PieDiagram,
  SequenceDiagram,
} from './diagram'
import { DiagramError } from './diagram'
import { diagramColor } from './diagram-color'

// Lays a parsed diagram out as a flat scene of paths and text. Renderers only map tones to
// theme colors, so the web and Android (DiagramLayout.kt) draw the same geometry.

export type PathCommand
  = | ['M', number, number]
    | ['L', number, number]
    | ['C', number, number, number, number, number, number]
    | ['Z']

/** Theme roles; renderers resolve each to a light or dark color. */
export type Tone
  = | 'canvas'
    | 'ink'
    | 'muted'
    | 'edge'
    | 'label'
    | `${'node' | 'terminal' | 'decision' | 'data' | 'note' | 'cluster'}${'' | '-line' | '-ink'}`
    | `series-${number}`

export type Paint = { tone: Tone } | { color: string }

export interface PathItem {
  kind: 'path'
  d: PathCommand[]
  fill?: Paint
  stroke?: Paint
  width?: number
  dash?: number[]
  opacity?: number
  link?: string
  /** Raised node shapes cast a soft shadow. */
  shadow?: boolean
}

export interface TextItem {
  kind: 'text'
  x: number
  /** Center of the text block; each line is centered on its own line box. */
  y: number
  lines: string[]
  size: number
  weight: number
  anchor: 'start' | 'middle' | 'end'
  fill: Paint
  link?: string
}

export type SceneItem = PathItem | TextItem

export interface Scene {
  width: number
  height: number
  items: SceneItem[]
  /** Short accessible description of the drawing. */
  label: string
}

export interface TextMeasure {
  width: (text: string, size: number, weight: number) => number
}

export function lineHeight(size: number) {
  return Math.round(size * 1.4)
}

const NODE_FONT = 14
const LABEL_FONT = 12
const TITLE_FONT = 16
const CLUSTER_FONT = 12
const WRAP = 200
const MARGIN = 12
const NODE_SEP = 36
const EDGE_SEP = 16
const RANK_GAP = 26
const CLUSTER_PAD = 16
const ARROW = 8
const MAX_LAYOUT_NODES = 4000
const MAX_LAYOUT_CONSTRAINTS = 10_000
const MAX_SCENE_SIDE = 32_768

export function layoutDiagram(diagram: Diagram, measure: TextMeasure): Scene {
  const scene = diagram.kind === 'graph'
    ? layoutGraph(diagram, measure)
    : diagram.kind === 'sequence'
      ? layoutSequence(diagram, measure)
      : layoutPie(diagram, measure)
  if (scene.width > MAX_SCENE_SIDE || scene.height > MAX_SCENE_SIDE)
    throw new DiagramError('This diagram is too large to draw.')
  return scene
}

/** Break on explicit newlines, then between words to fit `max`, then inside long words. */
export function wrapText(text: string, size: number, weight: number, max: number, measure: TextMeasure) {
  const lines: string[] = []
  for (const paragraph of text.split('\n')) {
    let line = ''
    for (const word of paragraph.split(/\s+/).filter(Boolean)) {
      const candidate = line ? `${line} ${word}` : word
      if (measure.width(candidate, size, weight) <= max) {
        line = candidate
        continue
      }

      if (line)
        lines.push(line)
      line = ''
      let start = 0
      while (start < word.length) {
        const remaining = word.length - start
        let low = 1
        let high = Math.min(2, remaining)
        // Grow from the start of each segment, never rescan the entire remaining word.
        while (high < remaining && measure.width(word.slice(start, start + high), size, weight) <= max) {
          low = high
          high = Math.min(high * 2, remaining)
        }

        if (measure.width(word.slice(start, start + high), size, weight) <= max) {
          low = high
        }
        else {
          while (high - low > 1) {
            const middle = Math.floor((low + high) / 2)
            if (measure.width(word.slice(start, start + middle), size, weight) <= max)
              low = middle
            else
              high = middle
          }
        }

        const segment = word.slice(start, start + low)
        start += low
        if (start < word.length)
          lines.push(segment)
        else
          line = segment
      }
    }

    lines.push(line)
  }

  while (lines.length > 1 && !lines.at(-1))
    lines.pop()
  return lines
}

interface TextBlock { lines: string[], width: number, height: number }

function textBlock(text: string, size: number, weight: number, max: number, measure: TextMeasure): TextBlock {
  const lines = text ? wrapText(text, size, weight, max, measure) : []
  const width = Math.max(0, ...lines.map(line => measure.width(line, size, weight)))
  return { lines, width, height: lines.length * lineHeight(size) }
}

// Path helpers. Quarter ellipse arcs use the usual cubic approximation.
const KAPPA = 0.5523

function roundedRect(x: number, y: number, w: number, h: number, radius: number): PathCommand[] {
  const r = Math.max(0, Math.min(radius, w / 2, h / 2))
  const k = r * (1 - KAPPA)
  return [
    ['M', x + r, y],
    ['L', x + w - r, y],
    ['C', x + w - k, y, x + w, y + k, x + w, y + r],
    ['L', x + w, y + h - r],
    ['C', x + w, y + h - k, x + w - k, y + h, x + w - r, y + h],
    ['L', x + r, y + h],
    ['C', x + k, y + h, x, y + h - k, x, y + h - r],
    ['L', x, y + r],
    ['C', x, y + k, x + k, y, x + r, y],
    ['Z'],
  ]
}

function ellipse(cx: number, cy: number, rx: number, ry: number): PathCommand[] {
  const ox = rx * KAPPA
  const oy = ry * KAPPA
  return [
    ['M', cx - rx, cy],
    ['C', cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry],
    ['C', cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy],
    ['C', cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry],
    ['C', cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy],
    ['Z'],
  ]
}

function polygon(points: [number, number][]): PathCommand[] {
  return [
    ['M', points[0]![0], points[0]![1]],
    ...points.slice(1).map(([x, y]) => ['L', x, y] as PathCommand),
    ['Z'],
  ]
}

/** Arc from angle a0 to a1 (radians, clockwise from 12 o'clock), as cubic segments. */
function arc(cx: number, cy: number, r: number, a0: number, a1: number, move: boolean): PathCommand[] {
  const commands: PathCommand[] = []
  const point = (angle: number) => [cx + r * Math.sin(angle), cy - r * Math.cos(angle)] as const
  const steps = Math.max(1, Math.ceil(Math.abs(a1 - a0) / (Math.PI / 2)))
  const step = (a1 - a0) / steps
  const handle = (4 / 3) * Math.tan(step / 4) * r
  let [x, y] = point(a0)
  commands.push(move ? ['M', x, y] : ['L', x, y])
  for (let index = 0; index < steps; index++) {
    const start = a0 + step * index
    const end = start + step
    const [ex, ey] = point(end)
    commands.push([
      'C',
      x + handle * Math.cos(start),
      y + handle * Math.sin(start),
      ex - handle * Math.cos(end),
      ey - handle * Math.sin(end),
      ex,
      ey,
    ])
    x = ex
    y = ey
  }

  return commands
}

// Author colors stay readable: text on a custom fill picks ink by the fill's luminance.
function luminance(color: string) {
  const argb = diagramColor(color)
  // A translucent fill depends on the theme beneath it; keep the theme's normal ink.
  if (argb === undefined || argb >>> 24 !== 255)
    return undefined
  const rgb = [(argb >>> 16) & 255, (argb >>> 8) & 255, argb & 255]
  const [r, g, b] = rgb.map((value) => {
    const channel = value / 255
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4
  })
  return 0.2126 * r! + 0.7152 * g! + 0.0722 * b!
}

function textOn(style: DiagramStyle | undefined, fallback: Tone): Paint {
  if (style?.color)
    return { color: style.color }
  if (style?.fill) {
    const light = luminance(style.fill)
    if (light !== undefined) {
      const darkInk = '#16151d'
      const darkContrast = (light + 0.05) / (luminance(darkInk)! + 0.05)
      const lightContrast = 1.05 / (light + 0.05)
      return { color: darkContrast >= lightContrast ? darkInk : '#ffffff' }
    }
  }

  return { tone: fallback }
}

type Family = 'node' | 'terminal' | 'decision' | 'data' | 'note'

function family(shape: NodeShape): Family {
  if (shape === 'stadium' || shape === 'circle' || shape === 'doublecircle')
    return 'terminal'
  if (shape === 'diamond' || shape === 'hexagon' || shape === 'choice')
    return 'decision'
  if (shape === 'cylinder')
    return 'data'
  if (shape === 'note')
    return 'note'
  return 'node'
}

interface NodeBox { w: number, h: number, text: TextBlock }

function nodeBox(node: GraphNode, horizontal: boolean, measure: TextMeasure): NodeBox {
  const text = textBlock(node.label, NODE_FONT, 500, WRAP, measure)
  const tw = text.width
  const th = Math.max(text.height, lineHeight(NODE_FONT))
  const box = (w: number, h: number) => ({ w: Math.ceil(w), h: Math.ceil(h), text })
  switch (node.shape) {
    case 'start':
      return box(16, 16)
    case 'end':
      return box(20, 20)
    case 'fork':
      return horizontal ? box(8, 64) : box(64, 8)
    case 'choice':
      return box(28, 28)
    case 'circle': {
      const d = Math.max(tw, th) + 28
      return box(d, d)
    }

    case 'doublecircle': {
      const d = Math.max(tw, th) + 38
      return box(d, d)
    }

    case 'diamond': {
      // Half extents (W, H) contain the label box when (tw/2)/W + (th/2)/H ≤ 1.
      const H = th / 2 + 22
      const W = (tw / 2 + 10) / (1 - (th / 2) / H)
      return box(Math.max(W * 2, 56), H * 2)
    }

    case 'hexagon':
      return box(tw + 40 + th, th + 22)
    case 'parallelogram':
    case 'parallelogram-alt':
    case 'trapezoid':
    case 'trapezoid-alt':
      return box(tw + 36 + (th + 22) * 0.7, th + 22)
    case 'cylinder':
      return box(Math.max(tw + 32, 64), th + 34)
    case 'subroutine':
      return box(tw + 48, th + 22)
    case 'flag':
      return box(tw + 44, th + 22)
    case 'stadium':
      return box(tw + 28 + th, th + 22)
    case 'note':
      return box(tw + 36, th + 24)
    default:
      return box(Math.max(tw + 32, 56), th + 22)
  }
}

/** Distance from a node's center to its bottom (or top) outline at horizontal offset dx. */
function outline(shape: NodeShape, w: number, h: number, dx: number) {
  const hw = w / 2
  const hh = h / 2
  const t = Math.min(1, Math.abs(dx) / hw)
  if (shape === 'diamond' || shape === 'choice')
    return hh * (1 - t)
  if (shape === 'circle' || shape === 'doublecircle' || shape === 'start' || shape === 'end')
    return hh * Math.sqrt(Math.max(0, 1 - t * t))
  if (shape === 'stadium') {
    const flat = hw - hh
    if (Math.abs(dx) <= flat)
      return hh
    const e = (Math.abs(dx) - flat) / hh
    return hh * Math.sqrt(Math.max(0, 1 - e * e))
  }

  if (shape === 'hexagon') {
    const inset = h / 2
    const flat = hw - inset
    return Math.abs(dx) <= flat ? hh : hh * (hw - Math.abs(dx)) / inset
  }

  return hh
}

function shapePaths(shape: NodeShape, cx: number, cy: number, w: number, h: number) {
  const x = cx - w / 2
  const y = cy - h / 2
  const r = x + w
  const b = y + h
  const outlines: PathCommand[][] = []
  const details: PathCommand[][] = []
  switch (shape) {
    case 'round':
      outlines.push(roundedRect(x, y, w, h, Math.min(16, h / 2)))
      break
    case 'stadium':
      outlines.push(roundedRect(x, y, w, h, h / 2))
      break
    case 'subroutine':
      outlines.push(roundedRect(x, y, w, h, 4))
      details.push([['M', x + 9, y], ['L', x + 9, b]], [['M', r - 9, y], ['L', r - 9, b]])
      break
    case 'cylinder': {
      const ry = 7
      const k = (w / 2) * KAPPA
      const ok = ry * KAPPA
      outlines.push([
        ['M', x, y + ry],
        ['L', x, b - ry],
        ['C', x, b - ry + ok, cx - k, b, cx, b],
        ['C', cx + k, b, r, b - ry + ok, r, b - ry],
        ['L', r, y + ry],
        ['C', r, y + ry - ok, cx + k, y, cx, y],
        ['C', cx - k, y, x, y + ry - ok, x, y + ry],
        ['Z'],
      ])
      details.push([
        ['M', x, y + ry],
        ['C', x, y + ry + ok, cx - k, y + 2 * ry, cx, y + 2 * ry],
        ['C', cx + k, y + 2 * ry, r, y + ry + ok, r, y + ry],
      ])
      break
    }

    case 'circle':
    case 'start':
      outlines.push(ellipse(cx, cy, w / 2, h / 2))
      break
    case 'doublecircle':
      outlines.push(ellipse(cx, cy, w / 2, h / 2))
      details.push(ellipse(cx, cy, w / 2 - 5, h / 2 - 5))
      break
    case 'end':
      outlines.push(ellipse(cx, cy, w / 2, h / 2))
      details.push(ellipse(cx, cy, w / 2 - 5, h / 2 - 5))
      break
    case 'diamond':
    case 'choice':
      outlines.push(polygon([[cx, y], [r, cy], [cx, b], [x, cy]]))
      break
    case 'hexagon': {
      const inset = h / 2
      outlines.push(polygon([[x + inset, y], [r - inset, y], [r, cy], [r - inset, b], [x + inset, b], [x, cy]]))
      break
    }

    case 'parallelogram':
    case 'parallelogram-alt':
    case 'trapezoid':
    case 'trapezoid-alt': {
      const s = h * 0.35
      const points: Record<string, [number, number][]> = {
        'parallelogram': [[x + s, y], [r, y], [r - s, b], [x, b]],
        'parallelogram-alt': [[x, y], [r - s, y], [r, b], [x + s, b]],
        'trapezoid': [[x + s, y], [r - s, y], [r, b], [x, b]],
        'trapezoid-alt': [[x, y], [r, y], [r - s, b], [x + s, b]],
      }
      outlines.push(polygon(points[shape]!))
      break
    }

    case 'flag':
      outlines.push(polygon([[x, y], [r, y], [r, b], [x, b], [x + h / 3, cy]]))
      break
    case 'note': {
      const fold = 12
      outlines.push(polygon([[x, y], [r - fold, y], [r, y + fold], [r, b], [x, b]]))
      details.push([['M', r - fold, y], ['L', r - fold, y + fold], ['L', r, y + fold]])
      break
    }

    case 'fork':
      outlines.push(roundedRect(x, y, w, h, Math.min(w, h) / 2))
      break
    default:
      outlines.push(roundedRect(x, y, w, h, 7))
  }

  return { outlines, details }
}

function headPaths(head: EdgeHead, tip: [number, number], direction: [number, number], paint: Paint, width: number): PathItem[] {
  const [ux, uy] = direction
  const [px, py] = [-uy, ux]
  const [tx, ty] = tip
  if (head === 'arrow') {
    const back = ARROW + width
    const half = 4.5 + width / 2
    const bx = tx - ux * back
    const by = ty - uy * back
    return [{
      kind: 'path',
      d: [
        ['M', tx, ty],
        ['L', bx + px * half, by + py * half],
        ['C', bx + ux * 2, by + uy * 2, bx + ux * 2, by + uy * 2, bx - px * half, by - py * half],
        ['Z'],
      ],
      fill: paint,
      stroke: paint,
      width: 1,
    }]
  }

  if (head === 'circle') {
    return [{
      kind: 'path',
      d: ellipse(tx - ux * 4.5, ty - uy * 4.5, 4, 4),
      fill: { tone: 'canvas' },
      stroke: paint,
      width: 1.5,
    }]
  }

  if (head === 'cross') {
    const cx = tx - ux * 5
    const cy = ty - uy * 5
    const s = 4
    return [{
      kind: 'path',
      d: [
        ['M', cx - (ux + px) * s, cy - (uy + py) * s],
        ['L', cx + (ux + px) * s, cy + (uy + py) * s],
        ['M', cx - (ux - px) * s, cy - (uy - py) * s],
        ['L', cx + (ux - px) * s, cy + (uy - py) * s],
      ],
      stroke: paint,
      width: 1.75,
    }]
  }

  return []
}

/** Space reserved between an edge's end and its head. */
function headInset(head: EdgeHead) {
  return head === 'arrow' ? ARROW : head === 'circle' ? 8.5 : head === 'cross' ? 1 : 0
}

// ---------------------------------------------------------------------------
// Layered graph layout (flowcharts and state diagrams).

type LKind = 'node' | 'dummy' | 'label' | 'left' | 'right'

interface LNode {
  key: string
  kind: LKind
  w: number
  h: number
  /** Innermost containing cluster. */
  cluster?: string
  rank: number
  order: number
  x: number
  y: number
  /** Extra room on the right for self loops. */
  extra: number
  /** Extra space along the rank axis for tall/wide loop labels, without moving shape ports. */
  rankPadding?: number
  node?: GraphNode
  box?: NodeBox
}

interface LEdge {
  edge: GraphEdge
  index: number
  u: string
  v: string
  minlen: number
  reversed: boolean
  label?: TextBlock
  chain: LNode[]
}

interface Box {
  x: number
  y: number
  w: number
  h: number
}

/** Include every drawn primitive, including loop labels that extend beyond ranked nodes. */
function fitScene(scene: Scene, measure: TextMeasure): Scene {
  let left = 0
  let top = 0
  let right = scene.width
  let bottom = scene.height
  const include = (x: number, y: number) => {
    if (x < 0)
      left = Math.min(left, x - MARGIN)
    if (y < 0)
      top = Math.min(top, y - MARGIN)
    if (x > scene.width)
      right = Math.max(right, x + MARGIN)
    if (y > scene.height)
      bottom = Math.max(bottom, y + MARGIN)
  }

  for (const item of scene.items) {
    if (item.kind === 'path') {
      const stroke = (item.width ?? 0) / 2
      for (const [, ...values] of item.d) {
        for (let index = 0; index < values.length; index += 2) {
          include(values[index]! - stroke, values[index + 1]! - stroke)
          include(values[index]! + stroke, values[index + 1]! + stroke)
        }
      }
    }
    else {
      const width = Math.max(0, ...item.lines.map(line => measure.width(line, item.size, item.weight)))
      const x = item.x - (item.anchor === 'middle' ? width / 2 : item.anchor === 'end' ? width : 0)
      const height = item.lines.length * lineHeight(item.size)
      include(x, item.y - height / 2)
      include(x + width, item.y + height / 2)
    }
  }

  if (!left && !top && right === scene.width && bottom === scene.height)
    return scene
  const items = scene.items.map((item): SceneItem => {
    if (item.kind === 'text')
      return { ...item, x: item.x - left, y: item.y - top }
    const d = item.d.map(([op, ...values]) => [op, ...values.map((value, index) => value - (index % 2 ? top : left))]) as PathCommand[]
    return { ...item, d }
  })
  return {
    ...scene,
    width: Math.ceil(right - left),
    height: Math.ceil(bottom - top),
    items,
  }
}

export function layoutGraph(graph: GraphDiagram, measure: TextMeasure): Scene {
  if (!graph.nodes.length && !graph.clusters.length)
    throw new DiagramError('This diagram has nothing to draw.')
  const horizontal = graph.direction === 'LR' || graph.direction === 'RL'
  const clusters = new Map(graph.clusters.map(cluster => [cluster.id, cluster]))
  const chains = new Map<string, string[]>()
  const chainOf = (id: string | undefined) => {
    if (!id)
      return []
    let chain = chains.get(id)
    if (!chain) {
      chain = []
      for (let current: string | undefined = id; current && clusters.has(current); current = clusters.get(current)!.parent)
        chain.unshift(current)
      chains.set(id, chain)
    }

    return chain
  }

  // Layout nodes, in layout space: x across ranks, y along them.
  const nodes = new Map<string, LNode>()
  const add = (node: LNode) => {
    nodes.set(node.key, node)
    return node
  }

  for (const node of graph.nodes) {
    const box = nodeBox(node, horizontal, measure)
    add({
      key: node.id,
      kind: 'node',
      w: horizontal ? box.h : box.w,
      h: horizontal ? box.w : box.h,
      cluster: node.parent,
      rank: 0,
      order: 0,
      x: 0,
      y: 0,
      extra: 0,
      node,
      box,
    })
  }

  const titles = new Map<string, TextBlock>()
  for (const cluster of graph.clusters)
    titles.set(cluster.id, textBlock(cluster.label, CLUSTER_FONT, 600, 240, measure))

  // An empty cluster still draws its box around a placeholder.
  function append<K, V>(map: Map<K, V[]>, key: K, value: V) {
    const values = map.get(key)
    if (values)
      values.push(value)
    else
      map.set(key, [value])
  }

  const members = new Map<string, string[]>()
  for (const node of graph.nodes) {
    for (const id of chainOf(node.parent))
      append(members, id, node.id)
  }

  for (const cluster of graph.clusters) {
    if (members.has(cluster.id))
      continue
    const key = `\u0000empty:${cluster.id}`
    const title = titles.get(cluster.id)!
    add({
      key,
      kind: 'dummy',
      w: horizontal ? 24 : Math.max(title.width, 48),
      h: 24,
      cluster: cluster.id,
      rank: 0,
      order: 0,
      x: 0,
      y: 0,
      extra: 0,
    })
    for (const id of chainOf(cluster.id))
      append(members, id, key)
  }

  // Rank constraints. A link to or from a subgraph constrains every member, so the whole
  // subgraph sits before or after the other end, as its box will be drawn.
  interface Constraint { u: string, v: string, minlen: number }

  const constraints: Constraint[] = []
  const ends = (id: string) => clusters.has(id) ? members.get(id)! : [id]
  const links: {
    edge: GraphEdge
    index: number
    label?: TextBlock
    back: boolean
  }[] = []
  const loops: {
    edge: GraphEdge
    index: number
    node: LNode
    label?: TextBlock
    reach: number
  }[] = []
  const nextLoopReach = new Map<string, number>()
  graph.edges.forEach((edge, index) => {
    const label = edge.label ? textBlock(edge.label, LABEL_FONT, 500, 160, measure) : undefined
    if (edge.from === edge.to) {
      const node = nodes.get(edge.from)
      if (node) {
        const reach = nextLoopReach.get(node.key) ?? 24 + Math.min(node.h, 40) / 2
        const labelExtent = label ? horizontal ? label.height : label.width : 0
        const rankExtent = label ? horizontal ? label.width : label.height : 0
        nextLoopReach.set(node.key, reach + Math.max(32, (labelExtent + 16) / 0.75))
        node.extra = Math.max(node.extra, reach + 12, reach * 0.75 + labelExtent + 18)
        node.rankPadding = Math.max(node.rankPadding ?? 0, (rankExtent - node.h) / 2 + 6)
        loops.push({
          edge,
          index,
          node,
          label,
          reach,
        })
      }

      return
    }

    links.push({
      edge,
      index,
      label,
      back: false,
    })
  })

  // Break cycles twice: first between links as written, with subgraphs as single ends, so a
  // link back into a subgraph is the one reversed; then between the members it expands to.
  const linkState = new Map<string, 1 | 2>()
  const linksFrom = new Map<string, typeof links>()
  for (const link of links)
    append(linksFrom, link.edge.from, link)
  for (const start of [...graph.nodes.map(node => node.id), ...clusters.keys()]) {
    if (linkState.has(start))
      continue
    const stack: [string, number][] = [[start, 0]]
    linkState.set(start, 1)
    while (stack.length) {
      const top = stack.at(-1)!
      const list = linksFrom.get(top[0]) ?? []
      if (top[1] >= list.length) {
        linkState.set(top[0], 2)
        stack.pop()
        continue
      }

      const link = list[top[1]++]!
      const seen = linkState.get(link.edge.to)
      if (seen === 1) {
        link.back = true
      }
      else if (!seen) {
        linkState.set(link.edge.to, 1)
        stack.push([link.edge.to, 0])
      }
    }
  }

  for (const link of links) {
    const [from, to] = link.back ? [link.edge.to, link.edge.from] : [link.edge.from, link.edge.to]
    let count = 0
    for (const u of ends(from)) {
      for (const v of ends(to)) {
        if (u !== v && count++ < 400) {
          if (constraints.length >= MAX_LAYOUT_CONSTRAINTS)
            throw new DiagramError('This diagram has too many expanded links to draw.')
          constraints.push({ u, v, minlen: link.edge.length * 2 })
        }
      }
    }
  }

  const outgoing = new Map<string, Constraint[]>()
  for (const constraint of constraints)
    append(outgoing, constraint.u, constraint)
  const state = new Map<string, 1 | 2>()
  const backward = new Set<Constraint>()
  for (const start of nodes.keys()) {
    if (state.has(start))
      continue
    const stack: [string, number][] = [[start, 0]]
    state.set(start, 1)
    while (stack.length) {
      const top = stack.at(-1)!
      const list = outgoing.get(top[0]) ?? []
      if (top[1] >= list.length) {
        state.set(top[0], 2)
        stack.pop()
        continue
      }

      const constraint = list[top[1]++]!
      const seen = state.get(constraint.v)
      if (seen === 1) {
        backward.add(constraint)
      }
      else if (!seen) {
        state.set(constraint.v, 1)
        stack.push([constraint.v, 0])
      }
    }
  }

  for (const constraint of backward)
    [constraint.u, constraint.v] = [constraint.v, constraint.u]

  // Rank by longest path, then pull nodes with more outgoing than incoming constraints down.
  const ins = new Map<string, Constraint[]>()
  const outs = new Map<string, Constraint[]>()
  for (const constraint of constraints) {
    append(ins, constraint.v, constraint)
    append(outs, constraint.u, constraint)
  }

  const indegree = new Map([...nodes.keys()].map(key => [key, ins.get(key)?.length ?? 0]))
  const queue = [...nodes.keys()].filter(key => !indegree.get(key))
  const topo: string[] = []
  while (queue.length) {
    const key = queue.shift()!
    topo.push(key)
    for (const constraint of outs.get(key) ?? []) {
      const next = nodes.get(constraint.v)!
      next.rank = Math.max(next.rank, nodes.get(key)!.rank + constraint.minlen)
      indegree.set(constraint.v, indegree.get(constraint.v)! - 1)
      if (!indegree.get(constraint.v))
        queue.push(constraint.v)
    }
  }

  for (const key of topo.toReversed()) {
    const out = outs.get(key) ?? []
    if (!out.length || out.length <= (ins.get(key)?.length ?? 0))
      continue
    const node = nodes.get(key)!
    node.rank = Math.min(...out.map(constraint => nodes.get(constraint.v)!.rank - constraint.minlen))
  }

  const lowest = Math.min(...[...nodes.values()].map(node => node.rank))
  for (const node of nodes.values())
    node.rank -= lowest

  // With ranks known, a subgraph's link attaches to its member nearest the other end.
  const nearest = (id: string, rank: number) => {
    if (!clusters.has(id))
      return id
    let best = members.get(id)![0]!
    for (const key of members.get(id)!) {
      if (Math.abs(nodes.get(key)!.rank - rank) < Math.abs(nodes.get(best)!.rank - rank))
        best = key
    }

    return best
  }

  const edges: LEdge[] = []
  const flats: LEdge[] = []
  for (const { edge, index, label } of links) {
    const middle = (id: string) => {
      const ranks = ends(id).map(key => nodes.get(key)!.rank)
      return ranks.reduce((sum, rank) => sum + rank, 0) / ranks.length
    }

    const u = nearest(edge.from, middle(edge.to))
    const v = nearest(edge.to, nodes.get(u)!.rank)
    if (u === v)
      continue
    const reversed = nodes.get(u)!.rank > nodes.get(v)!.rank
    const link: LEdge = {
      edge,
      index,
      u: reversed ? v : u,
      v: reversed ? u : v,
      minlen: edge.length * 2,
      reversed,
      label,
      chain: [],
    }
    if (nodes.get(u)!.rank === nodes.get(v)!.rank)
      flats.push(link)
    else
      edges.push(link)
  }

  // Each edge becomes a chain of dummies, one per intermediate rank; labels take the middle one.
  const lca = (a: string | undefined, b: string | undefined) => {
    const left = chainOf(a)
    const right = chainOf(b)
    let common: string | undefined
    for (let index = 0; index < Math.min(left.length, right.length) && left[index] === right[index]; index++)
      common = left[index]
    return common
  }

  for (const edge of edges) {
    const u = nodes.get(edge.u)!
    const v = nodes.get(edge.v)!
    const cluster = lca(u.cluster, v.cluster)
    const middle = u.rank + Math.floor((v.rank - u.rank) / 2)
    edge.chain = [u]
    for (let rank = u.rank + 1; rank < v.rank; rank++) {
      const labelled = edge.label && rank === middle
      const label = edge.label!
      edge.chain.push(add({
        key: `\u0000edge:${edge.index}:${rank}`,
        kind: labelled ? 'label' : 'dummy',
        w: labelled ? (horizontal ? label.height + 8 : label.width + 14) : 0,
        h: labelled ? (horizontal ? label.width + 14 : label.height + 8) : 0,
        cluster,
        rank,
        order: 0,
        x: 0,
        y: 0,
        extra: 0,
      }))
    }

    edge.chain.push(v)
  }

  // Clusters keep a left and right border node on every rank they span.
  const spans = new Map<string, [number, number]>()
  for (const node of nodes.values()) {
    for (const id of chainOf(node.cluster)) {
      const span = spans.get(id)
      spans.set(id, span ? [Math.min(span[0], node.rank), Math.max(span[1], node.rank)] : [node.rank, node.rank])
    }
  }

  for (const [id, [first, last]] of spans) {
    for (let rank = first; rank <= last; rank++) {
      for (const kind of ['left', 'right'] as const) {
        add({
          key: `\u0000${kind}:${id}:${rank}`,
          kind,
          w: 0,
          h: 0,
          cluster: id,
          rank,
          order: 0,
          x: 0,
          y: 0,
          extra: 0,
        })
      }
    }
  }

  // Long links through many ranks multiply layout work; such drawings would not be legible.
  if (nodes.size > MAX_LAYOUT_NODES)
    throw new DiagramError('This diagram is too large to draw.')

  const rankCount = Math.max(0, ...[...nodes.values()].map(node => node.rank)) + 1
  const layers: LNode[][] = Array.from({ length: rankCount }, () => [])
  const neighbors = new Map<string, { up: LNode[], down: LNode[] }>()
  for (const node of nodes.values())
    neighbors.set(node.key, { up: [], down: [] })
  for (const edge of edges) {
    for (let index = 1; index < edge.chain.length; index++) {
      neighbors.get(edge.chain[index - 1]!.key)!.down.push(edge.chain[index]!)
      neighbors.get(edge.chain[index]!.key)!.up.push(edge.chain[index - 1]!)
    }
  }

  // Initial order: depth-first from the declared nodes, so authored order mostly survives.
  const visited = new Set<string>()
  const visit = (start: LNode) => {
    const stack = [start]
    while (stack.length) {
      const node = stack.pop()!
      if (visited.has(node.key))
        continue
      visited.add(node.key)
      layers[node.rank]!.push(node)
      stack.push(...neighbors.get(node.key)!.down.toReversed())
    }
  }

  for (const node of nodes.values()) {
    if (node.kind === 'node' || node.kind === 'dummy')
      visit(node)
  }

  for (const node of nodes.values()) {
    if (!visited.has(node.key))
      visit(node)
  }

  const clusterDepth = (id: string | undefined) => chainOf(id).length
  const siblingOrder = new Map<string, number>()

  /** Sort one layer by key, keeping each cluster contiguous and bordered. */
  const arrange = (layer: LNode[], key: (node: LNode) => number) => {
    const place = (items: LNode[], depth: number, inside: string | undefined): LNode[] => {
      interface Entry { key: number, nodes: LNode[], cluster?: string }

      const entries: Entry[] = []
      const groups = new Map<string, LNode[]>()
      let left: LNode | undefined
      let right: LNode | undefined
      for (const node of items) {
        const chain = chainOf(node.cluster)
        if (chain.length === depth) {
          if (node.kind === 'left' && node.cluster === inside)
            left = node
          else if (node.kind === 'right' && node.cluster === inside)
            right = node
          else
            entries.push({ key: key(node), nodes: [node] })
        }
        else {
          const child = chain[depth]!
          groups.set(child, [...(groups.get(child) ?? []), node])
        }
      }

      for (const [cluster, group] of groups) {
        const keyed = group.filter(node => node.kind !== 'left' && node.kind !== 'right')
        const value = keyed.length ? keyed.reduce((sum, node) => sum + key(node), 0) / keyed.length : key(group[0]!)
        entries.push({ key: value, nodes: place(group, depth + 1, cluster), cluster })
      }

      entries.sort((a, b) => a.key - b.key)
      // Sibling clusters keep one relative order on every rank, or their boxes would overlap.
      const slots = entries.map((entry, index) => entry.cluster ? index : -1).filter(index => index >= 0)
      const ordered = slots.map(index => entries[index]!).sort((a, b) => (siblingOrder.get(a.cluster!) ?? 0) - (siblingOrder.get(b.cluster!) ?? 0))
      slots.forEach((slot, index) => {
        entries[slot] = ordered[index]!
      })
      return [...(left ? [left] : []), ...entries.flatMap(entry => entry.nodes), ...(right ? [right] : [])]
    }

    const sorted = place(layer, 0, undefined)
    sorted.forEach((node, index) => {
      node.order = index
    })
    return sorted
  }

  const updateSiblingOrder = () => {
    const totals = new Map<string, [number, number]>()
    for (const layer of layers) {
      for (const node of layer) {
        if (!node.cluster)
          continue
        for (const id of chainOf(node.cluster)) {
          const total = totals.get(id) ?? [0, 0]
          totals.set(id, [total[0] + node.order / Math.max(1, layer.length - 1), total[1] + 1])
        }
      }
    }

    for (const [id, [sum, count]] of totals)
      siblingOrder.set(id, sum / count)
  }

  // Crossings between two layers are inversions of the lower ends, counted with a Fenwick tree.
  const crossings = () => {
    let count = 0
    for (let rank = 0; rank + 1 < layers.length; rank++) {
      const ends: number[] = []
      for (const node of layers[rank]!) {
        const lower = neighbors.get(node.key)!.down.map(next => next.order).sort((a, b) => a - b)
        ends.push(...lower)
      }

      const size = layers[rank + 1]!.length + 1
      const tree = Array.from<number>({ length: size + 1 }).fill(0)
      for (const [index, end] of ends.entries()) {
        let below = 0
        for (let at = end + 1; at > 0; at -= at & -at)
          below += tree[at]!
        count += index - below
        for (let at = end + 1; at <= size; at += at & -at)
          tree[at]!++
      }
    }

    return count
  }

  for (const layer of layers) {
    layer.forEach((node, index) => {
      node.order = index
    })
  }

  updateSiblingOrder()
  for (const [rank, layer] of layers.entries())
    layers[rank] = arrange(layer, node => node.order)

  let best = layers.map(layer => [...layer])
  let bestCrossings = crossings()
  const barycenter = (node: LNode, side: 'up' | 'down') => {
    const list = neighbors.get(node.key)![side]
    if (!list.length)
      return node.order
    return list.reduce((sum, next) => sum + next.order, 0) / list.length
  }

  for (let iteration = 0; iteration < 16 && bestCrossings > 0; iteration++) {
    const down = iteration % 2 === 0
    updateSiblingOrder()
    const ranks = [...layers.keys()]
    for (const rank of down ? ranks.slice(1) : ranks.toReversed().slice(1))
      layers[rank] = arrange(layers[rank]!, node => barycenter(node, down ? 'up' : 'down'))
    const count = crossings()
    if (count < bestCrossings) {
      bestCrossings = count
      best = layers.map(layer => [...layer])
    }
  }

  for (const [rank, layer] of best.entries()) {
    layers[rank] = layer
    layer.forEach((node, index) => {
      node.order = index
    })
  }

  // Horizontal positions. Separation depends on what sits side by side.
  const separation = (a: LNode, b: LNode) => {
    const half = (a.w + b.w) / 2 + a.extra
    if (a.kind === 'left' && b.cluster === a.cluster && b.kind !== 'right')
      return half + CLUSTER_PAD + (horizontal ? titles.get(a.cluster!)!.height + 6 : 0)
    if (a.kind === 'left' || b.kind === 'right')
      return half + CLUSTER_PAD
    if (a.kind === 'right' || b.kind === 'left')
      return half + (a.kind === 'right' && b.kind === 'left' ? 14 : 20)
    if (a.kind === 'dummy' || b.kind === 'dummy')
      return half + EDGE_SEP
    return half + NODE_SEP
  }

  const weight = (a: LNode, b: LNode) => {
    const dummyA = a.kind === 'dummy' || a.kind === 'label'
    const dummyB = b.kind === 'dummy' || b.kind === 'label'
    return dummyA && dummyB ? 8 : dummyA || dummyB ? 2 : 1
  }

  const pushRight = () => {
    for (const layer of layers) {
      for (let index = 1; index < layer.length; index++) {
        const minimum = layer[index - 1]!.x + separation(layer[index - 1]!, layer[index]!)
        if (layer[index]!.x < minimum)
          layer[index]!.x = minimum
      }
    }
  }

  pushRight()

  /** Pool-adjacent-violators: closest positions to the targets that keep the separations. */
  const solve = (layer: LNode[], targets: number[], weights: number[]) => {
    const offsets = [0]
    for (let index = 1; index < layer.length; index++)
      offsets.push(offsets[index - 1]! + separation(layer[index - 1]!, layer[index]!))
    const blocks: { weight: number, total: number, count: number }[] = []
    for (let index = 0; index < layer.length; index++) {
      blocks.push({ weight: weights[index]!, total: weights[index]! * (targets[index]! - offsets[index]!), count: 1 })
      while (blocks.length > 1) {
        const last = blocks.at(-1)!
        const previous = blocks.at(-2)!
        if (previous.total / previous.weight <= last.total / last.weight)
          break
        blocks.pop()
        previous.weight += last.weight
        previous.total += last.total
        previous.count += last.count
      }
    }

    let index = 0
    for (const block of blocks) {
      const value = block.total / block.weight
      for (let item = 0; item < block.count; item++, index++)
        layer[index]!.x = value + offsets[index]!
    }
  }

  for (let iteration = 0; iteration < 10; iteration++) {
    const down = iteration % 2 === 0
    const ranks = [...layers.keys()]
    for (const rank of down ? ranks : ranks.toReversed()) {
      const layer = layers[rank]!
      const targets: number[] = []
      const weights: number[] = []
      for (const node of layer) {
        const links = neighbors.get(node.key)!
        const primary = down ? links.up : links.down
        const list = primary.length ? primary : (down ? links.down : links.up)
        if (!list.length) {
          targets.push(node.x)
          weights.push(node.kind === 'left' || node.kind === 'right' ? 0.25 : 0.5)
          continue
        }

        let total = 0
        let sum = 0
        for (const next of list) {
          const w = weight(node, next)
          total += w
          sum += w * next.x
        }

        targets.push(sum / total)
        weights.push(total)
      }

      solve(layer, targets, weights)
    }
  }

  // Align every cluster's borders across ranks by moving them outward only: left borders to
  // their leftmost rank, right borders to their rightmost. Whatever lies outside is pushed
  // away, so members never move and the passes settle.
  const borders = new Map<string, { left: LNode[], right: LNode[] }>()
  for (const node of nodes.values()) {
    if (node.kind !== 'left' && node.kind !== 'right')
      continue
    const entry = borders.get(node.cluster!) ?? { left: [], right: [] }
    entry[node.kind].push(node)
    borders.set(node.cluster!, entry)
  }

  const minimumWidth = (id: string) => (horizontal ? 0 : titles.get(id)!.width + CLUSTER_PAD * 2)
  for (let iteration = 0; iteration < 50; iteration++) {
    let changed = false
    for (const [id, { left, right }] of borders) {
      const l = Math.min(...left.map(node => node.x))
      const r = Math.max(...right.map(node => node.x), l + minimumWidth(id))
      for (const node of [...left, ...right]) {
        const x = node.kind === 'left' ? l : r
        if (node.x === x)
          continue
        node.x = x
        changed = true
        const layer = layers[node.rank]!
        if (node.kind === 'left') {
          for (let index = node.order - 1; index >= 0; index--)
            layer[index]!.x = Math.min(layer[index]!.x, layer[index + 1]!.x - separation(layer[index]!, layer[index + 1]!))
        }
        else {
          for (let index = node.order + 1; index < layer.length; index++)
            layer[index]!.x = Math.max(layer[index]!.x, layer[index - 1]!.x + separation(layer[index - 1]!, layer[index]!))
        }
      }
    }

    if (!changed)
      break
  }

  // Straighten long edges where their layers leave room for a direct line.
  const fits = (node: LNode, x: number) => {
    const layer = layers[node.rank]!
    const left = layer[node.order - 1]
    const right = layer[node.order + 1]
    return (!left || left.x + separation(left, node) <= x + 0.01) && (!right || x + separation(node, right) <= right.x + 0.01)
  }

  for (let pass = 0; pass < 2; pass++) {
    for (const edge of edges) {
      const inner = edge.chain.slice(1, -1)
      if (!inner.length)
        continue
      const a = edge.chain[0]!
      const b = edge.chain.at(-1)!
      const target = (node: LNode) => a.x + (b.x - a.x) * (node.rank - a.rank) / (b.rank - a.rank)
      if (inner.every(node => fits(node, target(node)))) {
        for (const node of inner)
          node.x = target(node)
      }
    }
  }

  // Center each subgraph's content when its title made the box wider.
  for (const cluster of [...graph.clusters].sort((a, b) => clusterDepth(b.id) - clusterDepth(a.id))) {
    const entry = borders.get(cluster.id)
    if (!entry)
      continue
    const inside = [...nodes.values()].filter(node => node.cluster && chainOf(node.cluster).includes(cluster.id) && !((node.kind === 'left' || node.kind === 'right') && node.cluster === cluster.id))
    if (!inside.length)
      continue
    const contentLeft = Math.min(...inside.map(node => node.x - node.w / 2))
    const contentRight = Math.max(...inside.map(node => node.x + node.w / 2 + node.extra))
    const leftRoom = contentLeft - entry.left[0]!.x - CLUSTER_PAD - (horizontal ? titles.get(cluster.id)!.height + 6 : 0)
    const rightRoom = entry.right[0]!.x - contentRight - CLUSTER_PAD
    const shift = (rightRoom - leftRoom) / 2
    if (shift > 0.5) {
      for (const node of inside)
        node.x += shift
    }
  }

  // Vertical positions: rank gaps grow where clusters open or close.
  const rankHeight = layers.map(layer => Math.max(0, ...layer.map(node => node.h + (node.rankPadding ?? 0) * 2)))
  const opening = Array.from<number>({ length: rankCount }).fill(0)
  const closing = Array.from<number>({ length: rankCount }).fill(0)
  for (const layer of layers) {
    for (const node of layer) {
      let opens = 0
      let closes = 0
      for (const id of chainOf(node.cluster)) {
        const [first, last] = spans.get(id)!
        if (first === node.rank)
          opens += CLUSTER_PAD + (graph.direction === 'TB' ? titles.get(id)!.height + 4 : 0)
        if (last === node.rank)
          closes += CLUSTER_PAD + (graph.direction === 'BT' ? titles.get(id)!.height + 4 : 0)
      }

      opening[node.rank] = Math.max(opening[node.rank]!, opens)
      closing[node.rank] = Math.max(closing[node.rank]!, closes)
    }
  }

  const rankY: number[] = []
  let cursor = 0
  for (let rank = 0; rank < rankCount; rank++) {
    cursor += (rank ? RANK_GAP : 0) + opening[rank]!
    rankY.push(cursor + rankHeight[rank]! / 2)
    cursor += rankHeight[rank]! + closing[rank]!
  }

  for (const node of nodes.values())
    node.y = rankY[node.rank]!

  // Cluster boxes, innermost first so parents can include their children.
  const boxes = new Map<string, Box>()
  const ordered = [...graph.clusters].sort((a, b) => clusterDepth(b.id) - clusterDepth(a.id))
  for (const cluster of ordered) {
    if (!spans.has(cluster.id))
      continue
    const { left, right } = borders.get(cluster.id)!
    let top = Infinity
    let bottom = -Infinity
    for (const node of nodes.values()) {
      if (node.kind === 'left' || node.kind === 'right' || !chainOf(node.cluster).includes(cluster.id))
        continue
      top = Math.min(top, node.y - node.h / 2 - (node.rankPadding ?? 0))
      bottom = Math.max(bottom, node.y + node.h / 2 + (node.rankPadding ?? 0))
    }

    for (const child of graph.clusters) {
      const box = child.parent === cluster.id ? boxes.get(child.id) : undefined
      if (box) {
        top = Math.min(top, box.y)
        bottom = Math.max(bottom, box.y + box.h)
      }
    }

    const title = titles.get(cluster.id)!.height + 4
    top -= CLUSTER_PAD + (graph.direction === 'TB' ? title : 0)
    bottom += CLUSTER_PAD + (graph.direction === 'BT' ? title : 0)
    boxes.set(cluster.id, {
      x: left[0]!.x,
      y: top,
      w: right[0]!.x - left[0]!.x,
      h: bottom - top,
    })
  }

  // Layout extents and the transform into the drawing's own direction.
  let minX = Infinity
  let maxX = -Infinity
  let maxY = 0
  for (const node of nodes.values()) {
    minX = Math.min(minX, node.x - node.w / 2)
    maxX = Math.max(maxX, node.x + node.w / 2 + node.extra)
    maxY = Math.max(maxY, node.y + node.h / 2 + (node.rankPadding ?? 0))
  }

  for (const box of boxes.values()) {
    minX = Math.min(minX, box.x)
    maxX = Math.max(maxX, box.x + box.w)
    maxY = Math.max(maxY, box.y + box.h)
  }

  const titleBlock = graph.title ? textBlock(graph.title, TITLE_FONT, 600, 480, measure) : undefined
  const titleSpace = titleBlock ? titleBlock.height + 14 : 0
  const extentX = maxX - minX
  const extentY = maxY
  const graphWidth = (horizontal ? extentY : extentX) + MARGIN * 2
  const titleWidth = titleBlock ? titleBlock.width + MARGIN * 2 : 0
  const horizontalMargin = MARGIN + Math.max(0, (titleWidth - graphWidth) / 2)
  const transform = ([x, y]: [number, number]): [number, number] => {
    const lx = x - minX
    switch (graph.direction) {
      case 'BT':
        return [lx + horizontalMargin, extentY - y + MARGIN + titleSpace]
      case 'LR':
        return [y + horizontalMargin, lx + MARGIN + titleSpace]
      case 'RL':
        return [extentY - y + horizontalMargin, lx + MARGIN + titleSpace]
      default:
        return [lx + horizontalMargin, y + MARGIN + titleSpace]
    }
  }

  const width = Math.ceil(Math.max(graphWidth, titleWidth))
  const height = Math.ceil((horizontal ? extentX : extentY) + MARGIN * 2 + titleSpace)
  const finalBox = (box: Box): Box => {
    const [x0, y0] = transform([box.x, box.y])
    const [x1, y1] = transform([box.x + box.w, box.y + box.h])
    return {
      x: Math.min(x0, x1),
      y: Math.min(y0, y1),
      w: Math.abs(x1 - x0),
      h: Math.abs(y1 - y0),
    }
  }

  const items: SceneItem[] = []
  const backgrounds: SceneItem[] = []
  const edgeItems: SceneItem[] = []
  const labelItems: SceneItem[] = []
  const titleItems: SceneItem[] = []
  const nodeItems: SceneItem[] = []

  if (titleBlock) {
    items.push({
      kind: 'text',
      x: width / 2,
      y: MARGIN + titleBlock.height / 2,
      lines: titleBlock.lines,
      size: TITLE_FONT,
      weight: 600,
      anchor: 'middle',
      fill: { tone: 'ink' },
    })
  }

  for (const cluster of [...graph.clusters].sort((a, b) => clusterDepth(a.id) - clusterDepth(b.id))) {
    const box = boxes.get(cluster.id)
    if (!box)
      continue
    const area = finalBox(box)
    backgrounds.push({
      kind: 'path',
      d: roundedRect(area.x, area.y, area.w, area.h, 14),
      fill: cluster.style?.fill ? { color: cluster.style.fill } : { tone: 'cluster' },
      stroke: cluster.style?.stroke ? { color: cluster.style.stroke } : { tone: 'cluster-line' },
      width: cluster.style?.strokeWidth ?? 1,
      ...(cluster.style?.dashed ? { dash: [5, 4] } : {}),
    })
    // Titles sit above edges, on a patch of the box's own fill, so lines never cross them.
    const title = titles.get(cluster.id)!
    if (title.lines.length) {
      const x = area.x + CLUSTER_PAD - 2
      const y = graph.direction === 'BT' ? area.y + area.h - 10 - title.height / 2 : area.y + 10 + title.height / 2
      titleItems.push(
        { kind: 'path', d: roundedRect(x - 4, y - title.height / 2 - 1, title.width + 8, title.height + 2, 5), fill: cluster.style?.fill ? { color: cluster.style.fill } : { tone: 'cluster' } },
        {
          kind: 'text',
          x,
          y,
          lines: title.lines,
          size: CLUSTER_FONT,
          weight: 600,
          anchor: 'start',
          fill: textOn(cluster.style, 'cluster-ink'),
        },
      )
    }
  }

  // Edges are cubic segments with tangents along the rank axis, joined at chain points.
  const axis = (sign: number): [number, number] => {
    switch (graph.direction) {
      case 'BT':
        return [0, -sign]
      case 'LR':
        return [sign, 0]
      case 'RL':
        return [-sign, 0]
      default:
        return [0, sign]
    }
  }

  const edgePaint = (edge: GraphEdge): Paint => edge.style?.stroke ? { color: edge.style.stroke } : { tone: 'edge' }
  const edgeWidth = (edge: GraphEdge) => edge.style?.strokeWidth ?? (edge.line === 'thick' ? 3 : 1.5)

  for (const edge of edges) {
    const { edge: source } = edge
    if (source.line === 'invisible')
      continue
    const chain = edge.chain
    const first = chain[0]!
    const last = chain.at(-1)!
    const port = (node: LNode, toward: number, side: 1 | -1): [number, number] => {
      if (!node.node)
        return [node.x, node.y]
      const shape = node.node.shape
      const round = shape === 'circle' || shape === 'doublecircle' || shape === 'diamond' || shape === 'choice' || shape === 'start' || shape === 'end'
      const reach = Math.max(0, round ? node.w * 0.2 : node.w / 2 - 12)
      const dx = Math.max(-reach, Math.min(reach, toward - node.x))
      // Transposed outlines only match for symmetric shapes; others leave from their flat side.
      const depth = horizontal && !round ? node.h / 2 : outline(shape, node.w, node.h, dx)
      return [node.x + dx, node.y + side * depth]
    }

    let points: [number, number][] = chain.slice(1, -1).map(node => [node.x, node.y])
    const startPoint = port(first, (chain[1] ?? last).x, 1)
    const endPoint = port(last, (chain.at(-2) ?? first).x, -1)
    points = [startPoint, ...points, endPoint]

    // Links to a subgraph stop at its border.
    const fromCluster = edge.reversed ? source.to : source.from
    const toCluster = edge.reversed ? source.from : source.to
    if (clusters.has(toCluster) && boxes.has(toCluster)) {
      const box = boxes.get(toCluster)!
      const outside = points.filter(([, y]) => y < box.y)
      const previous = outside.at(-1) ?? points[0]!
      points = [...outside, [Math.max(box.x + 12, Math.min(box.x + box.w - 12, previous[0])), box.y]]
    }

    if (clusters.has(fromCluster) && boxes.has(fromCluster)) {
      const box = boxes.get(fromCluster)!
      const outside = points.filter(([, y]) => y > box.y + box.h)
      const next = outside[0] ?? points.at(-1)!
      points = [[Math.max(box.x + 12, Math.min(box.x + box.w - 12, next[0])), box.y + box.h], ...outside]
    }

    if (points.length < 2)
      continue
    const startHead = edge.reversed ? source.end : source.start
    const endHead = edge.reversed ? source.start : source.end
    // Pull the curve back so heads sit at the outline instead of under the stroke.
    const startInset = headInset(startHead)
    const endInset = headInset(endHead)
    const tipStart = points[0]!
    const tipEnd = points.at(-1)!
    points[0] = [tipStart[0], tipStart[1] + startInset]
    points[points.length - 1] = [tipEnd[0], tipEnd[1] - endInset]

    const d = smoothPath(points, transform)
    const paint = edgePaint(source)
    const width = edgeWidth(source)
    const dashed = source.line === 'dotted' || source.style?.dashed
    edgeItems.push({
      kind: 'path',
      d,
      stroke: paint,
      width,
      ...(dashed ? { dash: [4, 4] } : {}),
    })
    edgeItems.push(...headPaths(endHead, transform(tipEnd), axis(1), paint, width))
    edgeItems.push(...headPaths(startHead, transform(tipStart), axis(-1), paint, width))

    const labelNode = chain.find(node => node.kind === 'label')
    if (edge.label) {
      const middle = points[Math.floor(points.length / 2)]!
      const before = points[Math.floor(points.length / 2) - 1] ?? middle
      const at: [number, number] = labelNode ? [labelNode.x, labelNode.y] : [(middle[0] + before[0]) / 2, (middle[1] + before[1]) / 2]
      labelItems.push(...edgeLabel(edge.label, transform(at)))
    }
  }

  // Links between nodes of one rank run straight across it.
  for (const edge of flats) {
    const { edge: source } = edge
    if (source.line === 'invisible')
      continue
    const [a, b] = [nodes.get(edge.u)!, nodes.get(edge.v)!].sort((left, right) => left.x - right.x) as [LNode, LNode]
    const forward = a.key === (edge.reversed ? edge.v : edge.u)
    const startHead = forward ? source.start : source.end
    const endHead = forward ? source.end : source.start
    const tipStart: [number, number] = [a.x + a.w / 2 + a.extra, a.y]
    const tipEnd: [number, number] = [b.x - b.w / 2, b.y]
    const from: [number, number] = [tipStart[0] + headInset(startHead), a.y]
    const to: [number, number] = [tipEnd[0] - headInset(endHead), b.y]
    const third = (to[0] - from[0]) / 3
    const paint = edgePaint(source)
    const width = edgeWidth(source)
    const [x0, y0] = transform([0, 0])
    const [x1, y1] = transform([1, 0])
    const across: [number, number] = [x1 - x0, y1 - y0]
    edgeItems.push({
      kind: 'path',
      d: [['M', ...transform(from)], ['C', ...transform([from[0] + third, from[1]]), ...transform([to[0] - third, to[1]]), ...transform(to)]],
      stroke: paint,
      width,
      ...(source.line === 'dotted' || source.style?.dashed ? { dash: [4, 4] } : {}),
    })
    edgeItems.push(...headPaths(endHead, transform(tipEnd), across, paint, width))
    edgeItems.push(...headPaths(startHead, transform(tipStart), [-across[0], -across[1]], paint, width))
    if (edge.label)
      labelItems.push(...edgeLabel(edge.label, transform([(from[0] + to[0]) / 2, a.y - edge.label.height / 2 - 6])))
  }

  for (const loop of loops) {
    const { node, edge } = loop
    const right = node.x + node.w / 2
    const reach = loop.reach
    const bend = (24 + Math.min(node.h, 40) / 2) * 0.7
    const top = node.y - Math.min(node.h / 4, 10)
    const bottom = node.y + Math.min(node.h / 4, 10)
    const inset = headInset(edge.end)
    const d: PathCommand[] = [
      ['M', ...transform([right + headInset(edge.start), top])],
      ['C', ...transform([right + reach, top - bend]), ...transform([right + reach, bottom + bend]), ...transform([right + inset, bottom])],
    ]
    const paint = edgePaint(edge)
    const width = edgeWidth(edge)
    if (edge.line !== 'invisible') {
      edgeItems.push({
        kind: 'path',
        d,
        stroke: paint,
        width,
        ...(edge.line === 'dotted' || edge.style?.dashed ? { dash: [4, 4] } : {}),
      })
      const [x0, y0] = transform([0, 0])
      const [x1, y1] = transform([-1, 0])
      edgeItems.push(...headPaths(edge.end, transform([right, bottom]), [x1 - x0, y1 - y0], paint, width))
      edgeItems.push(...headPaths(edge.start, transform([right, top]), [x1 - x0, y1 - y0], paint, width))
      if (loop.label) {
        const at = transform([right + reach * 0.75 + (horizontal ? loop.label.height : loop.label.width) / 2 + 6, node.y])
        labelItems.push(...edgeLabel(loop.label, at))
      }
    }
  }

  for (const node of nodes.values()) {
    if (!node.node || !node.box)
      continue
    const source = node.node
    const [cx, cy] = transform([node.x, node.y])
    const { w, h } = node.box
    const tone = family(source.shape)
    const style = source.style
    const solid = source.shape === 'start' || source.shape === 'fork'
    const { outlines, details } = shapePaths(source.shape, cx, cy, w, h)
    const fill: Paint = style?.fill ? { color: style.fill } : solid ? { tone: 'ink' } : { tone }
    const stroke: Paint = style?.stroke ? { color: style.stroke } : solid ? { tone: 'ink' } : { tone: `${tone}-line` }
    for (const d of outlines) {
      nodeItems.push({
        kind: 'path',
        d,
        fill,
        stroke,
        width: style?.strokeWidth ?? (source.shape === 'end' ? 1.5 : 1.25),
        ...(style?.dashed ? { dash: [5, 4] } : {}),
        ...(source.link ? { link: source.link } : {}),
        ...(solid || source.shape === 'end' ? {} : { shadow: true }),
      })
    }

    for (const d of details) {
      const filled = source.shape === 'end'
      nodeItems.push({
        kind: 'path',
        d,
        ...(filled ? { fill: { tone: 'ink' } as Paint } : {}),
        stroke: filled ? { tone: 'ink' } : stroke,
        width: 1.25,
        ...(source.link ? { link: source.link } : {}),
      })
    }

    if (node.box.text.lines.length && source.shape !== 'start' && source.shape !== 'end' && source.shape !== 'fork' && source.shape !== 'choice') {
      nodeItems.push({
        kind: 'text',
        x: cx,
        y: cy + (source.shape === 'cylinder' ? 5 : 0),
        lines: node.box.text.lines,
        size: NODE_FONT,
        weight: 500,
        anchor: 'middle',
        fill: textOn(style, `${tone}-ink`),
        ...(source.link ? { link: source.link } : {}),
      })
    }
  }

  items.push(...backgrounds, ...edgeItems, ...titleItems, ...labelItems, ...nodeItems)
  const count = graph.nodes.length
  return fitScene({
    width,
    height,
    items,
    label: graph.title ?? `Flowchart with ${count} ${count === 1 ? 'node' : 'nodes'}`,
  }, measure)
}

/**
 * A smooth curve through layout points. Ends leave along the rank axis; interior points take
 * Catmull-Rom tangents, so collinear runs stay straight.
 */
function smoothPath(points: [number, number][], transform: (point: [number, number]) => [number, number]): PathCommand[] {
  const tangents = points.map((point, index): [number, number] => {
    if (index === 0)
      return [0, (points[1]![1] - point[1]) * 1.5]
    if (index === points.length - 1)
      return [0, (point[1] - points[index - 1]![1]) * 1.5]
    const previous = points[index - 1]!
    const next = points[index + 1]!
    return [(next[0] - previous[0]) / 2, (next[1] - previous[1]) / 2]
  })
  const d: PathCommand[] = [['M', ...transform(points[0]!)]]
  for (let index = 1; index < points.length; index++) {
    const [ax, ay] = points[index - 1]!
    const [bx, by] = points[index]!
    const [mx, my] = tangents[index - 1]!
    const [nx, ny] = tangents[index]!
    d.push(['C', ...transform([ax + mx / 3, ay + my / 3]), ...transform([bx - nx / 3, by - ny / 3]), ...transform([bx, by])])
  }

  return d
}

function edgeLabel(label: TextBlock, [x, y]: [number, number]): SceneItem[] {
  const w = label.width + 12
  const h = label.height + 4
  return [
    { kind: 'path', d: roundedRect(x - w / 2, y - h / 2, w, h, 6), fill: { tone: 'label' } },
    {
      kind: 'text',
      x,
      y,
      lines: label.lines,
      size: LABEL_FONT,
      weight: 500,
      anchor: 'middle',
      fill: { tone: 'muted' },
    },
  ]
}

// ---------------------------------------------------------------------------
// Sequence diagrams.

const SEQ_FONT = 13
const ACTOR_GAP = 44

export function layoutSequence(diagram: SequenceDiagram, measure: TextMeasure): Scene {
  if (!diagram.participants.length)
    throw new DiagramError('This diagram has nothing to draw.')
  const items: SceneItem[] = []
  const titleBlock = diagram.title ? textBlock(diagram.title, TITLE_FONT, 600, 480, measure) : undefined
  const top = MARGIN + (titleBlock ? titleBlock.height + 14 : 0)
  const index = new Map(diagram.participants.map((participant, position) => [participant.id, position]))
  const heads = diagram.participants.map((participant) => {
    const text = textBlock(participant.label, NODE_FONT, 600, 150, measure)
    return {
      participant,
      text,
      w: Math.max(text.width + 32, 96),
      h: text.height + (participant.actor ? 48 : 22),
    }
  })
  const headHeight = Math.max(0, ...heads.map(head => head.h))

  // Column gaps grow until every message and note fits between its participants.
  const gaps = heads.slice(1).map((head, position) => (heads[position]!.w + head.w) / 2 + ACTOR_GAP)
  let leftRoom = 0
  let rightRoom = 0
  const need = (from: number, to: number, distance: number) => {
    if (from === to)
      return
    const [a, b] = from < to ? [from, to] : [to, from]
    let have = 0
    for (let column = a; column < b; column++)
      have += gaps[column]!
    if (have < distance)
      gaps[b - 1]! += distance - have
  }

  const prepared = diagram.items.map((item) => {
    if (item.type === 'message') {
      const text = textBlock(item.label, SEQ_FONT, 400, 260, measure)
      const from = index.get(item.from)!
      const to = index.get(item.to)!
      const numbered = item.number === undefined ? 0 : 22
      if (from === to) {
        if (from + 1 < heads.length)
          need(from, from + 1, text.width + 64 + numbered)
        else
          rightRoom = Math.max(rightRoom, text.width + 64 + numbered)
      }
      else {
        need(from, to, text.width + 36 + numbered)
      }

      return { item, text }
    }

    if (item.type === 'note') {
      const text = textBlock(item.label, SEQ_FONT, 400, 220, measure)
      const ids = item.participants.map(id => index.get(id)!)
      const w = text.width + 24
      if (item.placement === 'left') {
        if (ids[0]! > 0)
          need(ids[0]! - 1, ids[0]!, w + 24 + heads[ids[0]! - 1]!.w / 2)
        else
          leftRoom = Math.max(leftRoom, w + 12 - heads[0]!.w / 2)
      }
      else if (item.placement === 'right') {
        if (ids[0]! + 1 < heads.length)
          need(ids[0]!, ids[0]! + 1, w + 24 + heads[ids[0]! + 1]!.w / 2)
        else
          rightRoom = Math.max(rightRoom, w + 12 - heads.at(-1)!.w / 2)
      }
      else {
        const first = Math.min(...ids)
        const last = Math.max(...ids)
        if (first !== last) {
          need(first, last, w - 40)
        }
        else {
          const sideRoom = Math.max(0, (w - heads[first]!.w) / 2)
          if (first === 0)
            leftRoom = Math.max(leftRoom, sideRoom)
          else
            need(first - 1, first, w / 2 + heads[first - 1]!.w / 2 + 12)
          if (last === heads.length - 1)
            rightRoom = Math.max(rightRoom, sideRoom)
          else
            need(last, last + 1, w / 2 + heads[last + 1]!.w / 2 + 12)
        }
      }

      return { item, text }
    }

    if (item.type === 'block' || item.type === 'branch') {
      const label = item.label ? `[${item.label}]` : ''
      return { item, text: textBlock(label, LABEL_FONT, 500, 280, measure) }
    }

    return { item, text: undefined }
  })

  const centers: number[] = []
  let x = MARGIN + Math.max(0, leftRoom) + (heads[0]?.w ?? 0) / 2
  for (let column = 0; column < heads.length; column++) {
    centers.push(x)
    x += gaps[column] ?? 0
  }

  const right = (centers.at(-1) ?? 0) + (heads.at(-1)?.w ?? 0) / 2 + Math.max(0, rightRoom) + MARGIN

  // Rows.
  const rows: SceneItem[] = []
  const frames: SceneItem[] = []
  const bars: SceneItem[] = []
  const active = diagram.participants.map(() => [] as number[])
  const activations: {
    column: number
    start: number
    end: number
    depth: number
  }[] = []

  interface Frame {
    kind: string
    label?: TextBlock
    start: number
    branches: { y: number, label?: TextBlock, kind: string }[]
    columns: Set<number>
    color?: string
    depth: number
    low: number
    high: number
  }

  const stack: Frame[] = []
  const frameStartX = (frame: Frame) => {
    const columns = frame.columns.size ? [...frame.columns] : centers.map((_, column) => column)
    const columnLeft = Math.min(...columns.map(column => centers[column]!)) - 52 + frame.depth * 6
    return Math.max(MARGIN / 2 + frame.depth * 6, Math.min(columnLeft, frame.low - 14))
  }

  const finished: (Frame & { end: number })[] = []
  let y = top + headHeight + 26

  const touch = (...columns: number[]) => {
    for (const frame of stack) {
      for (const column of columns)
        frame.columns.add(column)
    }
  }

  // Frames also widen to the text drawn inside them.
  const extend = (low: number, high: number) => {
    for (const frame of stack) {
      frame.low = Math.min(frame.low, low)
      frame.high = Math.max(frame.high, high)
    }
  }

  const activate = (column: number, at: number) => {
    active[column]!.push(at)
  }

  const deactivate = (column: number, at: number) => {
    const start = active[column]!.pop()
    if (start !== undefined) {
      activations.push({
        column,
        start,
        end: at,
        depth: active[column]!.length,
      })
    }
  }

  const edgeX = (column: number, toward: number) => {
    const depth = active[column]!.length
    if (!depth)
      return centers[column]!
    return centers[column]! + (toward >= centers[column]! ? 5 : -5) + (depth - 1) * 4
  }

  for (const { item, text } of prepared) {
    if (item.type === 'message') {
      const from = index.get(item.from)!
      const to = index.get(item.to)!
      touch(from, to)
      const block = text!
      const paint: Paint = { tone: 'edge' }
      const dash = item.line === 'dotted' ? { dash: [5, 4] } : {}
      if (from === to) {
        const labelY = y + block.height / 2
        y += Math.max(block.height, 4) + 6
        const x0 = edgeX(from, Infinity)
        const loopTop = y
        const loopBottom = y + 22
        if (item.activate)
          activate(to, loopBottom - 4)
        const x1 = edgeX(to, Infinity)
        const inset = item.head === 'arrow' || item.head === 'async' ? ARROW : 0
        rows.push({
          kind: 'path',
          d: [
            ['M', x0, loopTop],
            ['L', x0 + 30, loopTop],
            ['C', x0 + 40, loopTop, x0 + 40, loopBottom, x0 + 30, loopBottom],
            ['L', x1 + inset, loopBottom],
          ],
          stroke: paint,
          width: 1.5,
          ...dash,
        })
        rows.push(...messageHead(item.head, [x1, loopBottom], [-1, 0], paint))
        if (item.both)
          rows.push(...messageHead(item.head, [x0, loopTop], [-1, 0], paint))
        rows.push(...messageText(block, x0 + 12 + (item.number === undefined ? 0 : 22), labelY, 'start'))
        extend(x0, Math.max(x0 + 44, x0 + 12 + (item.number === undefined ? 0 : 22) + block.width))
        if (item.number !== undefined)
          rows.push(...badge(item.number, x0 + 12, labelY, measure))
        y = loopBottom + 16
      }
      else {
        const labelY = y + block.height / 2
        y += Math.max(block.height, 4) + 6
        if (item.activate)
          activate(to, y)
        const direction = to > from ? 1 : -1
        const x0 = edgeX(from, centers[to]!)
        const x1 = edgeX(to, centers[from]!)
        const inset = item.head === 'arrow' || item.head === 'async' ? ARROW : item.head === 'cross' ? 2 : 0
        const startInset = item.both ? inset : 0
        rows.push({
          kind: 'path',
          d: [['M', x0 + direction * startInset, y], ['L', x1 - direction * inset, y]],
          stroke: paint,
          width: 1.5,
          ...dash,
        })
        rows.push(...messageHead(item.head, [x1, y], [direction, 0], paint))
        if (item.both)
          rows.push(...messageHead(item.head, [x0, y], [-direction, 0], paint))
        const middle = (x0 + x1) / 2
        rows.push(...messageText(block, middle + (item.number === undefined ? 0 : 11), labelY, 'middle'))
        extend(Math.min(x0, x1, middle - block.width / 2), Math.max(x0, x1, middle + block.width / 2 + 11))
        if (item.number !== undefined)
          rows.push(...badge(item.number, x0 + direction * 14, y, measure))
        y += 18
      }

      if (item.deactivate)
        deactivate(from, y - 18)
      continue
    }

    if (item.type === 'activate') {
      activate(index.get(item.participant)!, y - 12)
      continue
    }

    if (item.type === 'deactivate') {
      deactivate(index.get(item.participant)!, y - 12)
      continue
    }

    if (item.type === 'note') {
      const block = text!
      const ids = item.participants.map(id => index.get(id)!)
      touch(...ids)
      const w = block.width + 24
      const h = block.height + 14
      let left: number
      let width = w
      if (item.placement === 'left') {
        left = centers[ids[0]!]! - 14 - w
      }
      else if (item.placement === 'right') {
        left = centers[ids[0]!]! + 14
      }
      else {
        const a = Math.min(...ids.map(id => centers[id]!))
        const b = Math.max(...ids.map(id => centers[id]!))
        width = Math.max(w, b - a + 40)
        left = (a + b) / 2 - width / 2
      }

      rows.push({
        kind: 'path',
        d: polygon([[left, y], [left + width - 10, y], [left + width, y + 10], [left + width, y + h], [left, y + h]]),
        fill: { tone: 'note' },
        stroke: { tone: 'note-line' },
        width: 1,
      })
      rows.push({
        kind: 'path',
        d: [['M', left + width - 10, y], ['L', left + width - 10, y + 10], ['L', left + width, y + 10]],
        stroke: { tone: 'note-line' },
        width: 1,
      })
      rows.push({
        kind: 'text',
        x: left + width / 2,
        y: y + h / 2,
        lines: block.lines,
        size: SEQ_FONT,
        weight: 400,
        anchor: 'middle',
        fill: { tone: 'note-ink' },
      })
      extend(left, left + width)
      y += h + 14
      continue
    }

    if (item.type === 'block') {
      y += 4
      stack.push({
        kind: item.kind,
        label: text!.lines.length ? text! : undefined,
        start: y,
        branches: [],
        columns: new Set(),
        color: item.color,
        depth: stack.length,
        low: Infinity,
        high: -Infinity,
      })
      y += item.kind === 'rect' ? 12 : Math.max(24, (text?.height ?? 0) + 10)
      continue
    }

    if (item.type === 'branch') {
      const frame = stack.at(-1)
      if (frame) {
        y += 4
        frame.branches.push({ y, label: text!.lines.length ? text! : undefined, kind: item.kind })
        y += Math.max(22, (text?.height ?? 0) + 8)
      }

      continue
    }

    if (item.type === 'end') {
      const frame = stack.pop()
      if (frame) {
        y += 6
        if (frame.kind !== 'rect') {
          const left = frameStartX(frame)
          const tagWidth = measure.width(frame.kind, LABEL_FONT, 700) + 18
          const headerRight = left + tagWidth + 10 + (frame.label?.width ?? 0)
          const branchRight = left + 10 + Math.max(0, ...frame.branches.map(branch => branch.label?.width ?? 0))
          frame.high = Math.max(frame.high, headerRight, branchRight)
        }

        finished.push({ ...frame, end: y })
        for (const parent of stack) {
          for (const column of frame.columns)
            parent.columns.add(column)
          parent.low = Math.min(parent.low, frame.low - 8)
          parent.high = Math.max(parent.high, frame.high + 8)
        }

        y += 10
      }
    }
  }

  for (let column = 0; column < active.length; column++) {
    while (active[column]!.length)
      deactivate(column, y - 6)
  }

  const bottomTop = y + 8

  // Frames are drawn deepest last, inset by their depth so nested frames stay readable.
  let widest = right
  for (const frame of finished.sort((a, b) => a.depth - b.depth)) {
    const columns = frame.columns.size ? [...frame.columns] : centers.map((_, column) => column)
    const frameLeft = frameStartX(frame)
    let frameRight = Math.max(...columns.map(column => centers[column]!)) + 52 - frame.depth * 6
    frameRight = Math.max(frameRight, frameLeft + 140, frame.high + 14)
    widest = Math.max(widest, frameRight + MARGIN / 2)
    const left = frameLeft
    const right = frameRight
    if (frame.kind === 'rect') {
      frames.unshift({
        kind: 'path',
        d: roundedRect(left, frame.start, right - left, frame.end - frame.start, 8),
        fill: frame.color ? { color: frame.color } : { tone: 'cluster' },
        opacity: frame.color ? 0.22 : 1,
      })
      continue
    }

    frames.push({
      kind: 'path',
      d: roundedRect(left, frame.start, right - left, frame.end - frame.start, 8),
      stroke: { tone: 'cluster-line' },
      width: 1.25,
    })
    const tag = frame.kind
    const tagWidth = measure.width(tag, LABEL_FONT, 700) + 18
    frames.push({
      kind: 'path',
      d: [
        ['M', left + 8, frame.start],
        ['L', left + tagWidth, frame.start],
        ['L', left + tagWidth, frame.start + 14],
        ['L', left + tagWidth - 6, frame.start + 20],
        ['L', left, frame.start + 20],
        ['L', left, frame.start + 8],
        ['C', left, frame.start + 3.6, left + 3.6, frame.start, left + 8, frame.start],
        ['Z'],
      ],
      fill: { tone: 'cluster-line' },
    })
    frames.push({
      kind: 'text',
      x: left + 8,
      y: frame.start + 10,
      lines: [tag],
      size: LABEL_FONT,
      weight: 700,
      anchor: 'start',
      fill: { tone: 'cluster-ink' },
    })
    if (frame.label) {
      frames.push({
        kind: 'text',
        x: left + tagWidth + 10,
        y: frame.start + 10 + (frame.label.height - lineHeight(LABEL_FONT)) / 2,
        lines: frame.label.lines,
        size: LABEL_FONT,
        weight: 500,
        anchor: 'start',
        fill: { tone: 'muted' },
      })
    }

    for (const branch of frame.branches) {
      frames.push({
        kind: 'path',
        d: [['M', left, branch.y], ['L', right, branch.y]],
        stroke: { tone: 'cluster-line' },
        width: 1,
        dash: [5, 4],
      })
      if (branch.label) {
        frames.push({
          kind: 'text',
          x: left + 10,
          y: branch.y + 4 + branch.label.height / 2,
          lines: branch.label.lines,
          size: LABEL_FONT,
          weight: 500,
          anchor: 'start',
          fill: { tone: 'muted' },
        })
      }
    }
  }

  for (const activation of activations) {
    const xCenter = centers[activation.column]! + activation.depth * 4
    bars.push({
      kind: 'path',
      d: roundedRect(xCenter - 5, activation.start, 10, Math.max(12, activation.end - activation.start), 3),
      fill: { tone: 'data' },
      stroke: { tone: 'data-line' },
      width: 1,
    })
  }

  const lifelines: SceneItem[] = centers.map(center => ({
    kind: 'path',
    d: [['M', center, top + headHeight], ['L', center, bottomTop]],
    stroke: { tone: 'cluster-line' },
    width: 1.25,
    dash: [3, 4],
  }))

  const participants: SceneItem[] = []
  for (const [column, head] of heads.entries()) {
    for (const edge of [top, bottomTop]) {
      const cx = centers[column]!
      if (head.participant.actor) {
        const figureTop = edge + (headHeight - head.h) / 2
        participants.push(
          {
            kind: 'path',
            d: ellipse(cx, figureTop + 9, 7, 7),
            fill: { tone: 'terminal' },
            stroke: { tone: 'terminal-line' },
            width: 1.5,
          },
          {
            kind: 'path',
            d: [['M', cx - 13, figureTop + 34], ['C', cx - 13, figureTop + 22, cx + 13, figureTop + 22, cx + 13, figureTop + 34], ['Z']],
            fill: { tone: 'terminal' },
            stroke: { tone: 'terminal-line' },
            width: 1.5,
          },
          {
            kind: 'text',
            x: cx,
            y: figureTop + 40 + head.text.height / 2,
            lines: head.text.lines,
            size: NODE_FONT,
            weight: 600,
            anchor: 'middle',
            fill: { tone: 'ink' },
          },
        )
        continue
      }

      participants.push(
        {
          kind: 'path',
          d: roundedRect(cx - head.w / 2, edge, head.w, headHeight, 10),
          fill: { tone: 'terminal' },
          stroke: { tone: 'terminal-line' },
          width: 1.25,
          shadow: true,
        },
        {
          kind: 'text',
          x: cx,
          y: edge + headHeight / 2,
          lines: head.text.lines,
          size: NODE_FONT,
          weight: 600,
          anchor: 'middle',
          fill: { tone: 'terminal-ink' },
        },
      )
    }
  }

  const width = Math.ceil(Math.max(widest, titleBlock ? titleBlock.width + MARGIN * 2 : 0))
  if (titleBlock) {
    items.push({
      kind: 'text',
      x: width / 2,
      y: MARGIN + titleBlock.height / 2,
      lines: titleBlock.lines,
      size: TITLE_FONT,
      weight: 600,
      anchor: 'middle',
      fill: { tone: 'ink' },
    })
  }

  items.push(...frames.filter(item => item.kind === 'path' && item.opacity !== undefined), ...lifelines, ...bars, ...frames.filter(item => !(item.kind === 'path' && item.opacity !== undefined)), ...rows, ...participants)
  const count = diagram.participants.length
  return {
    width,
    height: Math.ceil(bottomTop + headHeight + MARGIN),
    items,
    label: diagram.title ?? `Sequence diagram with ${count} ${count === 1 ? 'participant' : 'participants'}`,
  }
}

function messageText(block: TextBlock, x: number, y: number, anchor: 'start' | 'middle'): SceneItem[] {
  if (!block.lines.length)
    return []
  return [{
    kind: 'text',
    x,
    y,
    lines: block.lines,
    size: SEQ_FONT,
    weight: 400,
    anchor,
    fill: { tone: 'ink' },
  }]
}

function badge(value: number, x: number, y: number, measure: TextMeasure): SceneItem[] {
  const text = String(value)
  const w = Math.max(18, measure.width(text, 10, 700) + 10)
  return [
    { kind: 'path', d: roundedRect(x - w / 2, y - 9, w, 18, 9), fill: { tone: 'edge' } },
    {
      kind: 'text',
      x,
      y,
      lines: [text],
      size: 10,
      weight: 700,
      anchor: 'middle',
      fill: { tone: 'canvas' },
    },
  ]
}

function messageHead(head: string, tip: [number, number], direction: [number, number], paint: Paint): SceneItem[] {
  const [tx, ty] = tip
  const [ux] = direction
  if (head === 'arrow')
    return headPaths('arrow', tip, direction, paint, 1.5)
  if (head === 'cross')
    return headPaths('cross', [tx + ux * 3, ty], direction, paint, 1.5)
  if (head === 'async') {
    return [{
      kind: 'path',
      d: [['M', tx - ux * 9, ty - 5], ['L', tx, ty], ['L', tx - ux * 9, ty + 5]],
      stroke: paint,
      width: 1.5,
    }]
  }

  return []
}

// ---------------------------------------------------------------------------
// Pie charts: a donut with a legend beside it.

const SERIES = 8

export function layoutPie(diagram: PieDiagram, measure: TextMeasure): Scene {
  const items: SceneItem[] = []
  const titleBlock = diagram.title ? textBlock(diagram.title, TITLE_FONT, 600, 480, measure) : undefined
  const top = MARGIN + (titleBlock ? titleBlock.height + 16 : 0)
  const total = diagram.slices.reduce((sum, slice) => sum + slice.value, 0)
  const radius = 92
  const inner = 56
  const cx = MARGIN + radius
  const visible = diagram.slices.filter(slice => slice.value > 0)
  const rowHeight = 24
  const legendHeight = visible.length * rowHeight
  const cy = top + Math.max(radius, legendHeight / 2)
  const format = (value: number) => (Math.round(value * 100) / 100).toLocaleString('en-US')

  let angle = 0
  visible.forEach((slice, position) => {
    const sweep = (slice.value / total) * Math.PI * 2
    const paint: Paint = { tone: `series-${position % SERIES}` }
    // Slices are separated by a hairline gap of equal width on both rims.
    const gap = visible.length === 1 ? 0 : Math.min(0.012, sweep / 4)
    items.push({
      kind: 'path',
      d: [...arc(cx, cy, radius, angle + gap, angle + sweep - gap, true), ...arc(cx, cy, inner, angle + sweep - gap * radius / inner, angle + gap * radius / inner, false), ['Z']],
      fill: paint,
    })

    const share = slice.value / total
    if (share >= 0.06) {
      const middle = angle + sweep / 2
      const ring = (radius + inner) / 2
      items.push({
        kind: 'text',
        x: cx + ring * Math.sin(middle),
        y: cy - ring * Math.cos(middle),
        lines: [`${Math.round(share * 100)}%`],
        size: 11,
        weight: 700,
        anchor: 'middle',
        fill: { tone: 'canvas' },
      })
    }

    angle += sweep
  })

  items.push({
    kind: 'text',
    x: cx,
    y: cy - 8,
    lines: [format(total)],
    size: 18,
    weight: 700,
    anchor: 'middle',
    fill: { tone: 'ink' },
  })
  items.push({
    kind: 'text',
    x: cx,
    y: cy + 12,
    lines: ['total'],
    size: 11,
    weight: 500,
    anchor: 'middle',
    fill: { tone: 'muted' },
  })

  const legendX = cx + radius + 32
  let legendY = cy - legendHeight / 2 + rowHeight / 2
  let legendWidth = 0
  for (const [position, slice] of visible.entries()) {
    const share = Math.round((slice.value / total) * 1000) / 10
    const label = slice.label
    const detail = diagram.showData ? `${share}% · ${format(slice.value)}` : `${share}%`
    items.push({ kind: 'path', d: roundedRect(legendX, legendY - 6, 12, 12, 3.5), fill: { tone: `series-${position % SERIES}` } })
    items.push({
      kind: 'text',
      x: legendX + 20,
      y: legendY,
      lines: [label],
      size: 13,
      weight: 500,
      anchor: 'start',
      fill: { tone: 'ink' },
    })
    const labelWidth = measure.width(label, 13, 500)
    items.push({
      kind: 'text',
      x: legendX + 20 + labelWidth + 8,
      y: legendY,
      lines: [detail],
      size: 12,
      weight: 500,
      anchor: 'start',
      fill: { tone: 'muted' },
    })
    legendWidth = Math.max(legendWidth, 20 + labelWidth + 8 + measure.width(detail, 12, 500))
    legendY += rowHeight
  }

  const width = Math.ceil(Math.max(legendX + legendWidth + MARGIN, titleBlock ? titleBlock.width + MARGIN * 2 : 0))
  if (titleBlock) {
    items.unshift({
      kind: 'text',
      x: width / 2,
      y: MARGIN + titleBlock.height / 2,
      lines: titleBlock.lines,
      size: TITLE_FONT,
      weight: 600,
      anchor: 'middle',
      fill: { tone: 'ink' },
    })
  }

  return {
    width,
    height: Math.ceil(Math.max(cy + radius, cy + legendHeight / 2) + MARGIN),
    items,
    label: diagram.title ?? `Pie chart with ${visible.length} ${visible.length === 1 ? 'slice' : 'slices'}`,
  }
}
