import type {
  Paint,
  PathCommand,
  Scene,
  TextMeasure,
} from './diagram-layout'
import { ref } from 'vue'
import { diagramKind, parseDiagram } from './diagram'
import { layoutDiagram, lineHeight } from './diagram-layout'
import {
  ChartPie,
  Code,
  Copy,
  Maximize2,
  MoveHorizontal,
  Workflow,
} from './icons'

const SVG = 'http://www.w3.org/2000/svg'
export const DIAGRAM_FONT = '"DM Sans Variable", system-ui, sans-serif'

const widths = new Map<string, number>()

/** Increments when web fonts finish loading, so measured layouts are redone once. */
export const diagramFonts = ref(0)
if (typeof document !== 'undefined' && document.fonts) {
  document.fonts.addEventListener('loadingdone', () => {
    widths.clear()
    diagramFonts.value++
  })
}

let context: CanvasRenderingContext2D | null | undefined

export const canvasMeasure: TextMeasure = {
  width(text, size, weight) {
    const key = `${size}:${weight}:${text}`
    const cached = widths.get(key)
    if (cached !== undefined)
      return cached
    context ??= document.createElement('canvas').getContext('2d')
    let width = text.length * size * 0.56
    if (context) {
      context.font = `${weight} ${size}px ${DIAGRAM_FONT}`
      width = context.measureText(text).width
    }

    if (widths.size > 4096)
      widths.clear()
    widths.set(key, width)
    return width
  },
}

export function pathData(commands: PathCommand[]) {
  const round = (value: number) => Math.round(value * 100) / 100
  return commands.map(([op, ...values]) => op + values.map(round).join(' ')).join('')
}

function paint(value: Paint | undefined) {
  if (!value)
    return 'none'
  return 'tone' in value ? `var(--dg-${value.tone})` : value.color
}

/** Draw a scene. With `resolve`, theme variables become literal colors for export. */
export function sceneSvg(scene: Scene, resolve?: (tone: string) => string) {
  const svg = document.createElementNS(SVG, 'svg')
  svg.setAttribute('viewBox', `0 0 ${scene.width} ${scene.height}`)
  svg.setAttribute('width', String(scene.width))
  svg.setAttribute('height', String(scene.height))
  svg.setAttribute('role', 'img')
  svg.setAttribute('aria-label', scene.label)
  svg.setAttribute('class', 'diagram-svg')
  const color = (value: Paint | undefined) => value && 'tone' in value && resolve ? resolve(value.tone) : paint(value)

  // Consecutive outlines, decorations and text of a linked node share one focusable link.
  let currentLink: SVGAElement | undefined

  function append(element: SVGElement, href?: string) {
    if (!href) {
      currentLink = undefined
      svg.append(element)
      return
    }

    if (currentLink?.getAttribute('href') !== href) {
      currentLink = document.createElementNS(SVG, 'a')
      currentLink.setAttribute('href', href)
      currentLink.setAttribute('target', '_blank')
      currentLink.setAttribute('rel', 'noopener noreferrer')
      currentLink.setAttribute('class', 'diagram-link')
      svg.append(currentLink)
    }

    currentLink!.append(element)
  }

  for (const item of scene.items) {
    if (item.kind === 'path') {
      const path = document.createElementNS(SVG, 'path')
      path.setAttribute('d', pathData(item.d))
      path.style.fill = color(item.fill)
      path.style.stroke = color(item.stroke)
      if (item.width)
        path.style.strokeWidth = String(item.width)
      if (item.dash)
        path.style.strokeDasharray = item.dash.join(' ')
      if (item.opacity !== undefined)
        path.style.opacity = String(item.opacity)
      if (item.shadow)
        path.setAttribute('class', 'diagram-raised')
      append(path, item.link)

      continue
    }

    const text = document.createElementNS(SVG, 'text')
    text.setAttribute('text-anchor', item.anchor)
    text.style.fill = color(item.fill)
    text.style.fontSize = `${item.size}px`
    text.style.fontWeight = String(item.weight)
    if (resolve)
      text.style.fontFamily = DIAGRAM_FONT
    const height = lineHeight(item.size)
    item.lines.forEach((line, index) => {
      const span = document.createElementNS(SVG, 'tspan')
      span.setAttribute('x', String(Math.round(item.x * 100) / 100))
      span.setAttribute('y', String(Math.round((item.y + (index - (item.lines.length - 1) / 2) * height) * 100) / 100))
      span.setAttribute('dominant-baseline', 'central')
      span.textContent = line
      text.append(span)
    })
    append(text, item.link)
  }

  return svg
}

const KINDS = {
  flowchart: { label: 'Flowchart', icon: Workflow },
  state: { label: 'State diagram', icon: Workflow },
  sequence: { label: 'Sequence diagram', icon: MoveHorizontal },
  pie: { label: 'Pie chart', icon: ChartPie },
} as const

const MAX_SCENES = 32

/** Owned by one Markdown component; unchanged diagrams keep their parsed layout. */
export class DiagramCache {
  private entries = new Map<string, Scene | Error>()
  private previous = new Map<string, Scene | Error>()
  private fonts = diagramFonts.value

  beginRender() {
    // Layouts measured with fallback fonts are discarded once the real fonts arrive.
    const fonts = diagramFonts.value
    this.previous = fonts === this.fonts ? this.entries : new Map()
    this.fonts = fonts
    this.entries = new Map()
  }

  scene(source: string) {
    let scene = this.entries.get(source) ?? this.previous.get(source)
    if (!scene) {
      try {
        scene = layoutDiagram(parseDiagram(source), canvasMeasure)
      }
      catch (error) {
        scene = error instanceof Error ? error : new Error(String(error))
      }
    }

    if (this.entries.size < MAX_SCENES)
      this.entries.set(source, scene)
    return scene
  }
}

function icon(name: string) {
  const element = document.createElement('i')
  element.setAttribute('aria-hidden', 'true')
  element.className = `ui-icon inline-block size-4 shrink-0 not-italic ${name}`
  return element
}

function action(name: string, label: string, iconName: string) {
  const button = document.createElement('button')
  button.type = 'button'
  button.className = 'diagram-action'
  button.dataset.diagramAction = name
  button.setAttribute('aria-label', label)
  button.title = label
  button.append(icon(iconName))
  return button
}

/**
 * A figure for a supported diagram, or undefined to keep the code block. The source stays in
 * the figure, so copying and the source view need no other state.
 */
export function diagramFigure(source: string, cache: DiagramCache, showSource: boolean) {
  const kind = diagramKind(source)
  if (!kind)
    return undefined
  const scene = cache.scene(source)
  const figure = document.createElement('figure')
  figure.className = 'diagram'
  figure.dataset.diagram = kind
  if (showSource)
    figure.classList.add('diagram-showing-source')

  const bar = document.createElement('figcaption')
  bar.className = 'diagram-bar'
  const title = document.createElement('span')
  title.className = 'diagram-kind'
  title.append(icon(KINDS[kind].icon), KINDS[kind].label)
  const actions = document.createElement('span')
  actions.className = 'diagram-actions'
  const sourceButton = action('source', 'Show source', Code)
  sourceButton.setAttribute('aria-pressed', String(showSource))
  actions.append(sourceButton, action('copy', 'Copy source', Copy))
  if (!(scene instanceof Error))
    actions.append(action('expand', 'Open diagram', Maximize2))
  bar.append(title, actions)

  const canvas = document.createElement('div')
  canvas.className = 'diagram-canvas'
  if (scene instanceof Error) {
    figure.classList.add('diagram-failed', 'diagram-showing-source')
    const notice = document.createElement('p')
    notice.className = 'diagram-error'
    notice.setAttribute('role', 'note')
    notice.textContent = `Couldn’t draw this diagram. ${scene.message}`
    canvas.append(notice)
  }
  else {
    const svg = sceneSvg(scene)
    // Wide drawings shrink to the message, but not below a legible size; they scroll instead.
    svg.style.width = `${scene.width}px`
    svg.style.maxWidth = '100%'
    svg.style.minWidth = `${Math.min(scene.width, Math.max(320, scene.width * 0.75))}px`
    canvas.append(svg)
  }

  const pre = document.createElement('pre')
  pre.className = 'diagram-source'
  const code = document.createElement('code')
  code.textContent = source
  pre.append(code)
  figure.append(bar, canvas, pre)
  return figure
}

export function sceneFor(source: string): Scene | undefined {
  try {
    return layoutDiagram(parseDiagram(source), canvasMeasure)
  }
  catch {
    return undefined
  }
}
