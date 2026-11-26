import type {
  Paint,
  Scene,
  SceneItem,
  TextBlock,
  TextMeasure,
} from './scene'
import type { SequenceDiagram } from './types'
import {
  ACTOR_GAP,
  ARROW,
  ellipse,
  fitScene,
  headPaths,
  LABEL_FONT,
  lineHeight,
  MARGIN,
  NODE_FONT,
  polygon,
  roundedRect,
  SEQ_FONT,
  textBlock,
  TITLE_FONT,
} from './scene'
import { DiagramError } from './types'

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

  const depths = diagram.participants.map(() => 0)
  const peakDepths = diagram.participants.map(() => 0)
  const changeDepth = (participant: string, delta: number) => {
    const column = index.get(participant)!
    depths[column] = Math.max(0, depths[column]! + delta)
    peakDepths[column] = Math.max(peakDepths[column]!, depths[column]!)
  }

  for (const item of diagram.items) {
    if (item.type === 'activate' || item.type === 'deactivate') {
      changeDepth(item.participant, item.type === 'activate' ? 1 : -1)
    }
    else if (item.type === 'message') {
      if (item.activate)
        changeDepth(item.to, 1)
      if (item.deactivate)
        changeDepth(item.from, -1)
    }
  }

  const activationReach = peakDepths.map(depth => depth ? (depth - 1) * 4 + 5.5 : 0)

  // Column gaps grow until every message and note fits between its participants.
  const gaps = heads.slice(1).map((head, position) => Math.max(heads[position]!.w / 2, activationReach[position]!) + head.w / 2 + ACTOR_GAP)
  let leftRoom = 0
  let rightRoom = Math.max(0, activationReach.at(-1)! - heads.at(-1)!.w / 2)
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
      const numbered = item.number === undefined ? 0 : sequenceBadgeWidth(item.number, measure) + 10
      if (from === to) {
        if (from + 1 < heads.length)
          need(from, from + 1, text.width + 64 + numbered + activationReach[from]!)
        else
          rightRoom = Math.max(rightRoom, text.width + 64 + numbered + activationReach[from]!)
      }
      else {
        need(from, to, text.width + 36 + numbered + activationReach[from]! + activationReach[to]!)
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
  const framePadding = 14
  const frameStartX = (frame: Frame) => {
    const columns = frame.columns.size ? [...frame.columns] : centers.map((_, column) => column)
    const columnLeft = Math.min(...columns.map(column => centers[column]!)) - 52 + frame.depth * 6
    return Math.max(MARGIN / 2 + frame.depth * 6, Math.min(columnLeft, frame.low - framePadding))
  }

  const frameBounds = (frame: Frame) => {
    const columns = frame.columns.size ? [...frame.columns] : centers.map((_, column) => column)
    const left = frameStartX(frame)
    const columnRight = Math.max(...columns.map(column => centers[column]!)) + 52 - frame.depth * 6
    return { left, right: Math.max(columnRight, left + 140, frame.high + framePadding) }
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
    const x = centers[column]! + active[column]!.length * 4
    touch(column)
    extend(x - 5.5, x + 5.5)
    active[column]!.push(at)
  }

  const deactivate = (column: number, at: number) => {
    const start = active[column]!.pop()
    if (start !== undefined) {
      const x = centers[column]! + active[column]!.length * 4
      touch(column)
      extend(x - 5.5, x + 5.5)
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
        const badgeWidth = item.number === undefined ? 0 : sequenceBadgeWidth(item.number, measure)
        const labelX = x0 + 12 + (item.number === undefined ? 0 : badgeWidth + 10)
        rows.push(...messageText(block, labelX, labelY, 'start'))
        extend(x0, Math.max(x0 + 44, labelX + block.width))
        if (item.number !== undefined)
          rows.push(...badge(item.number, x0 + 12 + badgeWidth / 2, labelY, measure))
        y = loopBottom + 16
      }
      else {
        const labelY = y + block.height / 2
        y += Math.max(block.height, 4) + (item.number === undefined ? 6 : 14)
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
        const labelX = middle + (item.number === undefined ? 0 : 11)
        const badgeX = x0 + direction * 14
        const badgeHalfWidth = item.number === undefined ? 0 : sequenceBadgeWidth(item.number, measure) / 2
        rows.push(...messageText(block, labelX, labelY, 'middle'))
        extend(Math.min(x0, x1, labelX - block.width / 2, badgeX - badgeHalfWidth), Math.max(x0, x1, labelX + block.width / 2, badgeX + badgeHalfWidth))
        if (item.number !== undefined)
          rows.push(...badge(item.number, badgeX, y, measure))
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

        // Include the drawn minimum width and unused columns before enclosing this child.
        const bounds = frameBounds(frame)
        frame.low = Math.min(frame.low, bounds.left + framePadding)
        frame.high = Math.max(frame.high, bounds.right - framePadding)
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
    const { left, right } = frameBounds(frame)
    widest = Math.max(widest, right + MARGIN / 2)
    if (frame.kind === 'rect') {
      frames.push({
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
  return fitScene({
    width,
    height: Math.ceil(bottomTop + headHeight + MARGIN),
    items,
    label: diagram.title ?? `Sequence diagram with ${count} ${count === 1 ? 'participant' : 'participants'}`,
  }, measure)
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

function sequenceBadgeWidth(value: number, measure: TextMeasure): number {
  return Math.max(18, measure.width(String(value), 10, 700) + 10)
}

function badge(value: number, x: number, y: number, measure: TextMeasure): SceneItem[] {
  const text = String(value)
  const w = sequenceBadgeWidth(value, measure)
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
