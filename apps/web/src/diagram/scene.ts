import type {
  DiagramStyle,
  Direction,
  EdgeHead,
  GraphNode,
  NodeShape,
} from './types'
import { DIAGRAM_LAYOUT } from '../../../../packages/contracts/generated/diagram-spec'
import { diagramColor } from '../diagram-color'
import { DiagramError } from './types'

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

export const NODE_FONT = DIAGRAM_LAYOUT.nodeFont
export const LABEL_FONT = DIAGRAM_LAYOUT.labelFont
export const TITLE_FONT = DIAGRAM_LAYOUT.titleFont
export const CLUSTER_FONT = DIAGRAM_LAYOUT.clusterFont
export const WRAP = DIAGRAM_LAYOUT.wrap
export const MARGIN = DIAGRAM_LAYOUT.margin
export const NODE_SEP = DIAGRAM_LAYOUT.nodeSep
export const EDGE_SEP = DIAGRAM_LAYOUT.edgeSep
export const RANK_GAP = DIAGRAM_LAYOUT.rankGap
export const CLUSTER_PAD = DIAGRAM_LAYOUT.clusterPad
export const ARROW = DIAGRAM_LAYOUT.arrow
export const MAX_LAYOUT_NODES = DIAGRAM_LAYOUT.maxLayoutNodes
export const MAX_LAYOUT_CONSTRAINTS = DIAGRAM_LAYOUT.maxLayoutConstraints
export const MAX_SCENE_SIDE = DIAGRAM_LAYOUT.maxSceneSide
export const KAPPA = DIAGRAM_LAYOUT.kappa
export const SEQ_FONT = DIAGRAM_LAYOUT.seqFont
export const ACTOR_GAP = DIAGRAM_LAYOUT.actorGap
export const SERIES = DIAGRAM_LAYOUT.series

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
      const offsets = [0]
      for (const character of word)
        offsets.push(offsets.at(-1)! + character.length)
      const characters = offsets.length - 1
      let start = 0
      while (start < characters) {
        const remaining = characters - start
        let low = 1
        let high = Math.min(2, remaining)
        // Grow from the start of each segment, never rescan the entire remaining word.
        while (high < remaining && measure.width(word.slice(offsets[start], offsets[start + high]), size, weight) <= max) {
          low = high
          high = Math.min(high * 2, remaining)
        }

        if (measure.width(word.slice(offsets[start], offsets[start + high]), size, weight) <= max) {
          low = high
        }
        else {
          while (high - low > 1) {
            const middle = Math.floor((low + high) / 2)
            if (measure.width(word.slice(offsets[start], offsets[start + middle]), size, weight) <= max)
              low = middle
            else
              high = middle
          }
        }

        const segment = word.slice(offsets[start], offsets[start + low])
        start += low
        if (start < characters)
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

export interface TextBlock { lines: string[], width: number, height: number }

export function textBlock(text: string, size: number, weight: number, max: number, measure: TextMeasure): TextBlock {
  const lines = text ? wrapText(text, size, weight, max, measure) : []
  const width = Math.max(0, ...lines.map(line => measure.width(line, size, weight)))
  return { lines, width, height: lines.length * lineHeight(size) }
}

// Path helpers. Quarter ellipse arcs use the usual cubic approximation.

export function roundedRect(x: number, y: number, w: number, h: number, radius: number): PathCommand[] {
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

export function ellipse(cx: number, cy: number, rx: number, ry: number): PathCommand[] {
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

export function polygon(points: [number, number][]): PathCommand[] {
  return [
    ['M', points[0]![0], points[0]![1]],
    ...points.slice(1).map(([x, y]) => ['L', x, y] as PathCommand),
    ['Z'],
  ]
}

/** Arc from angle a0 to a1 (radians, clockwise from 12 o'clock), as cubic segments. */
export function arc(cx: number, cy: number, r: number, a0: number, a1: number, move: boolean): PathCommand[] {
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
export function luminance(color: string) {
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

export function textOn(style: DiagramStyle | undefined, fallback: Tone): Paint {
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

export type Family = 'node' | 'terminal' | 'decision' | 'data' | 'note'

export function family(shape: NodeShape): Family {
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

export interface NodeBox { w: number, h: number, text: TextBlock }

export function nodeBox(node: GraphNode, horizontal: boolean, measure: TextMeasure): NodeBox {
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
      const d = Math.sqrt(tw * tw + th * th) + 28
      return box(d, d)
    }

    case 'doublecircle': {
      const d = Math.sqrt(tw * tw + th * th) + 38
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
      // Keep the centered label beyond the inward notch, whose depth is h / 3.
      return box(tw + 44 + (th + 22) * 2 / 3, th + 22)
    case 'stadium':
      return box(tw + 28 + th, th + 22)
    case 'note':
      return box(tw + 36, th + 24)
    default:
      return box(Math.max(tw + 32, 56), th + 22)
  }
}

export function shapePaths(shape: NodeShape, cx: number, cy: number, w: number, h: number) {
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

/** Intersect canonical node contours with a ray in the graph's layout coordinates. */
export function outlineExtents(outlines: PathCommand[][], direction: Direction, axis: 'x' | 'y', side: 1 | -1, offsets: number[]) {
  const project = (x: number, y: number): [number, number] => {
    const across = direction === 'LR' || direction === 'RL' ? y : x
    const rank = direction === 'BT' ? -y : direction === 'LR' ? x : direction === 'RL' ? -x : y
    return axis === 'x' ? [side * across, rank] : [side * rank, across]
  }

  const cubic = (a: number, b: number, c: number, d: number, t: number) => {
    const u = 1 - t
    return u * u * u * a + 3 * u * u * t * b + 3 * u * t * t * c + t * t * t * d
  }

  return offsets.map((offset) => {
    let extent = -Infinity
    for (const path of outlines) {
      let current: [number, number] = [0, 0]
      let first = current
      const advance = (end: [number, number], c1?: [number, number], c2?: [number, number]) => {
        if (offset >= Math.min(current[1], end[1]) && offset <= Math.max(current[1], end[1])) {
          if (current[1] === end[1]) {
            extent = Math.max(extent, current[0], end[0])
          }
          else if (c1 && c2) {
            // Generated outline cubics are monotone on both axes; bisection stays bounded.
            let low = 0
            let high = 1
            for (let iteration = 0; iteration < 32; iteration++) {
              const middle = (low + high) / 2
              const value = cubic(current[1], c1[1], c2[1], end[1], middle)
              if ((value < offset) === (end[1] > current[1]))
                low = middle
              else
                high = middle
            }

            extent = Math.max(extent, cubic(current[0], c1[0], c2[0], end[0], (low + high) / 2))
          }
          else {
            const t = (offset - current[1]) / (end[1] - current[1])
            extent = Math.max(extent, current[0] + (end[0] - current[0]) * t)
          }
        }

        current = end
      }

      for (const command of path) {
        switch (command[0]) {
          case 'M':
            first = current = project(command[1], command[2])
            break
          case 'L':
            advance(project(command[1], command[2]))
            break
          case 'C':
            advance(project(command[5], command[6]), project(command[1], command[2]), project(command[3], command[4]))
            break
          case 'Z':
            advance(first)
            break
        }
      }
    }

    if (!Number.isFinite(extent))
      throw new DiagramError('This node outline cannot be drawn.')
    return extent
  })
}

export function headPaths(head: EdgeHead, tip: [number, number], direction: [number, number], paint: Paint, width: number): PathItem[] {
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
export function headInset(head: EdgeHead) {
  return head === 'arrow' ? ARROW : head === 'circle' ? 8.5 : head === 'cross' ? 1 : 0
}

/** Include every drawn primitive, including loop labels that extend beyond ranked nodes. */
export function fitScene(scene: Scene, measure: TextMeasure): Scene {
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
