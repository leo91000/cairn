import type { Paint, Scene, TextMeasure } from './diagram-layout'
import fontStyles from '@fontsource-variable/dm-sans/wght.css?inline'
import { parseDiagram } from './diagram'
import { layoutDiagram, lineHeight } from './diagram-layout'
import { DIAGRAM_FONT, pathData, sceneSvg } from './diagram-svg'

let embeddedFonts: Promise<string> | undefined

async function embedFonts() {
  const sheet = new CSSStyleSheet()
  sheet.replaceSync(fontStyles)
  const faces = [...sheet.cssRules].filter((rule): rule is CSSFontFaceRule => rule instanceof CSSFontFaceRule)
  const css = await Promise.all(faces.map(async (face) => {
    const source = face.style.getPropertyValue('src')
    const start = source.indexOf('url(')
    const end = source.indexOf(')', start)
    if (start < 0 || end < 0)
      throw new Error('The diagram font has no downloadable source.')
    let url = source.slice(start + 4, end).trim()
    if ((url.startsWith('"') && url.endsWith('"')) || (url.startsWith('\'') && url.endsWith('\'')))
      url = url.slice(1, -1)
    const response = await fetch(url)
    if (!response.ok)
      throw new Error('The diagram font could not be loaded.')
    const blob = new Blob([await response.arrayBuffer()], { type: 'font/woff2' })
    const data = await new Promise<string>((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => typeof reader.result === 'string' ? resolve(reader.result) : reject(new Error('Invalid diagram font data.'))
      reader.onerror = () => reject(reader.error ?? new Error('The diagram font could not be read.'))
      reader.readAsDataURL(blob)
    })
    face.style.setProperty('src', `url("${data}") format("woff2")`)
    face.style.setProperty('font-display', 'block')
    return face.cssText
  }))
  if (!css.length)
    throw new Error('The diagram font is unavailable.')
  return css.join('\n')
}

function fontCss() {
  embeddedFonts ??= embedFonts().catch((error) => {
    embeddedFonts = undefined
    throw error
  })
  return embeddedFonts
}

async function exportScene(source: string) {
  await Promise.all([400, 500, 600, 700].map(weight => document.fonts.load(`${weight} 15px ${DIAGRAM_FONT}`, source)))
  await document.fonts.ready
  const context = document.createElement('canvas').getContext('2d')
  if (!context)
    throw new Error('Text measurement is unavailable.')
  const measure: TextMeasure = {
    width(text, size, weight) {
      context.font = `${weight} ${size}px ${DIAGRAM_FONT}`
      return context.measureText(text).width
    },
  }
  return { scene: layoutDiagram(parseDiagram(source), measure), measure }
}

/** Standalone SVGs carry their fonts; PNG draws directly with the page's loaded typefaces. */
export async function diagramExport(source: string, format: 'svg' | 'png', color: (tone: string) => string): Promise<Blob> {
  const { scene, measure } = await exportScene(source)
  if (format === 'png')
    return rasterize(scene, color)

  const svg = sceneSvg(scene, color)
  const style = document.createElementNS('http://www.w3.org/2000/svg', 'style')
  style.textContent = await fontCss()
  svg.prepend(style)
  svg.setAttribute('xmlns', 'http://www.w3.org/2000/svg')
  // Retain geometry even in SVG readers that substitute another font for unsupported glyphs.
  for (const span of svg.querySelectorAll('tspan')) {
    const text = span.parentElement!
    const width = measure.width(span.textContent ?? '', Number.parseFloat(text.style.fontSize), Number(text.style.fontWeight))
    if (width > 0) {
      span.setAttribute('textLength', String(width))
      span.setAttribute('lengthAdjust', 'spacingAndGlyphs')
    }
  }

  const background = document.createElementNS('http://www.w3.org/2000/svg', 'rect')
  background.setAttribute('width', '100%')
  background.setAttribute('height', '100%')
  background.setAttribute('fill', color('canvas'))
  svg.prepend(background)
  return new Blob([new XMLSerializer().serializeToString(svg)], { type: 'image/svg+xml' })
}

function rasterize(scene: Scene, color: (tone: string) => string): Promise<Blob> {
  const ratio = Math.min(2, Math.sqrt(16_000_000 / (scene.width * scene.height)), 8192 / Math.max(scene.width, scene.height))
  const canvas = document.createElement('canvas')
  canvas.width = Math.max(1, Math.round(scene.width * ratio))
  canvas.height = Math.max(1, Math.round(scene.height * ratio))
  const ctx = canvas.getContext('2d')
  if (!ctx)
    throw new Error('Image rendering is unavailable.')
  ctx.fillStyle = color('canvas')
  ctx.fillRect(0, 0, canvas.width, canvas.height)
  ctx.scale(canvas.width / scene.width, canvas.height / scene.height)
  ctx.lineJoin = 'miter'
  ctx.miterLimit = 4
  const paint = (value: Paint) => 'tone' in value ? color(value.tone) : value.color
  for (const item of scene.items) {
    ctx.save()
    if (item.kind === 'path') {
      const path = new Path2D(pathData(item.d))
      ctx.globalAlpha = Math.max(0, Math.min(1, item.opacity ?? 1))
      if (item.fill && paint(item.fill).toLowerCase() !== 'none') {
        ctx.fillStyle = '#000'
        ctx.fillStyle = paint(item.fill)
        ctx.fill(path)
      }

      if (item.stroke && paint(item.stroke).toLowerCase() !== 'none') {
        ctx.strokeStyle = 'transparent'
        ctx.strokeStyle = paint(item.stroke)
        ctx.lineWidth = item.width || 1
        ctx.setLineDash(item.dash ?? [])
        ctx.stroke(path)
      }
    }
    else if (paint(item.fill).toLowerCase() !== 'none') {
      ctx.font = `${item.weight} ${item.size}px ${DIAGRAM_FONT}`
      ctx.textAlign = item.anchor === 'middle' ? 'center' : item.anchor === 'end' ? 'right' : 'left'
      ctx.textBaseline = 'middle'
      ctx.fillStyle = '#000'
      ctx.fillStyle = paint(item.fill)
      item.lines.forEach((line, index) => ctx.fillText(line, item.x, item.y + (index - (item.lines.length - 1) / 2) * lineHeight(item.size)))
    }

    ctx.restore()
  }

  return new Promise((resolve, reject) => canvas.toBlob(blob => blob ? resolve(blob) : reject(new Error('PNG encoding failed.')), 'image/png'))
}
