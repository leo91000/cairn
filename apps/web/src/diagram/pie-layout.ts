import type {
  Paint,
  Scene,
  SceneItem,
  TextMeasure,
} from './scene'
import type { PieDiagram } from './types'
import {
  arc,
  MARGIN,
  roundedRect,
  SERIES,
  textBlock,
  TITLE_FONT,
} from './scene'
import { DiagramError } from './types'

export function layoutPie(diagram: PieDiagram, measure: TextMeasure): Scene {
  const items: SceneItem[] = []
  const titleBlock = diagram.title ? textBlock(diagram.title, TITLE_FONT, 600, 480, measure) : undefined
  const top = MARGIN + (titleBlock ? titleBlock.height + 16 : 0)
  const total = diagram.slices.reduce((sum, slice) => sum + slice.value, 0)
  if (!Number.isFinite(total))
    throw new DiagramError('The pie total is too large to draw.')
  const radius = 92
  const inner = 56
  const cx = MARGIN + radius
  const positiveCount = diagram.slices.filter(slice => slice.value > 0).length
  const rowHeight = 24
  const format = (value: number) => value > 0 && value < 0.01 ? String(value) : (value < 1e12 ? Math.round(value * 100) / 100 : value).toLocaleString('en-US')
  const totalText = format(total)
  const totalInLegend = measure.width(totalText, 18, 700) > inner * 2 - 24
  const legendHeight = (diagram.slices.length + (totalInLegend ? 1 : 0)) * rowHeight
  const cy = top + Math.max(radius, legendHeight / 2)

  let angle = 0
  diagram.slices.forEach((slice, position) => {
    if (slice.value === 0)
      return
    const sweep = (slice.value / total) * Math.PI * 2
    const paint: Paint = { tone: `series-${position % SERIES}` }
    // Slices are separated by a hairline gap of equal width on both rims.
    const gap = positiveCount === 1 ? 0 : Math.min(0.012, sweep / 4)
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
    lines: [totalInLegend ? 'Σ' : totalText],
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
  for (const [position, slice] of diagram.slices.entries()) {
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

  if (totalInLegend) {
    const label = `Total: ${totalText}`
    items.push({
      kind: 'text',
      x: legendX + 20,
      y: legendY,
      lines: [label],
      size: 12,
      weight: 600,
      anchor: 'start',
      fill: { tone: 'ink' },
    })
    legendWidth = Math.max(legendWidth, 20 + measure.width(label, 12, 600))
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
    label: diagram.title ?? `Pie chart with ${positiveCount} ${positiveCount === 1 ? 'slice' : 'slices'}`,
  }
}
