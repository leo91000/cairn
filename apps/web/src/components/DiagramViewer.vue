<script setup lang="ts">
import type { Scene } from '../diagram-layout'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import { apiResourceUrl } from '../api'
import { artifactLink } from '../deliverables'
import { diagramKind } from '../diagram'
import { diagramExport } from '../diagram-export'
import { sceneFor, sceneSvg } from '../diagram-svg'
import {
  Check,
  Copy,
  Download,
  Scan,
  ZoomIn,
  ZoomOut,
} from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import Modal from './Modal.vue'

const props = defineProps<{
  source: string
  returnFocus?: HTMLElement
  openingLink?: boolean
  linkError?: string
}>()
const emit = defineEmits<{ close: [], openLink: [event: MouseEvent] }>()

const TITLES = {
  flowchart: 'Flowchart',
  state: 'State diagram',
  sequence: 'Sequence diagram',
  pie: 'Pie chart',
} as const
const title = TITLES[diagramKind(props.source) ?? 'flowchart']
const scene: Scene | undefined = sceneFor(props.source)
const stage = ref<HTMLElement>()
const drawing = ref<HTMLElement>()
const scale = ref(1)
const x = ref(0)
const y = ref(0)
const copied = ref(false)
const exporting = ref(false)
const exportError = ref('')
const percent = computed(() => `${Math.round(scale.value * 100)}%`)
const pointers = new Map<number, { x: number, y: number, dragging: boolean }>()
let suppressClick = false
let pinch: {
  distance: number
  scale: number
  x: number
  y: number
} | undefined
let observer: ResizeObserver | undefined
let fitted = true
let copiedTimer: ReturnType<typeof setTimeout> | undefined

function fitScale() {
  const area = stage.value
  if (!area || !scene || !area.clientWidth || !area.clientHeight)
    return undefined
  const padding = Math.min(32, area.clientWidth / 4, area.clientHeight / 4)
  return Math.min((area.clientWidth - padding * 2) / scene.width, (area.clientHeight - padding * 2) / scene.height, 2)
}

function clamp(value: number) {
  const minimum = Math.min(0.15, fitScale() ?? 0.15)
  return Math.min(6, Math.max(minimum, value))
}

function fit() {
  const area = stage.value
  const next = fitScale()
  if (!area || !scene || next === undefined)
    return
  scale.value = next
  x.value = (area.clientWidth - scene.width * next) / 2
  y.value = (area.clientHeight - scene.height * next) / 2
  fitted = true
}

/** Zoom around a point of the stage, keeping the drawing under it in place. */
function zoomAt(next: number, px: number, py: number) {
  const target = clamp(next)
  const ratio = target / scale.value
  x.value = px - (px - x.value) * ratio
  y.value = py - (py - y.value) * ratio
  scale.value = target
  fitted = false
}

function zoomBy(factor: number) {
  const area = stage.value
  if (area)
    zoomAt(scale.value * factor, area.clientWidth / 2, area.clientHeight / 2)
}

function onWheel(event: WheelEvent) {
  event.preventDefault()
  const bounds = stage.value!.getBoundingClientRect()
  if (event.ctrlKey || event.metaKey || !event.deltaX) {
    zoomAt(scale.value * Math.exp(-event.deltaY * (event.ctrlKey ? 0.01 : 0.0015)), event.clientX - bounds.left, event.clientY - bounds.top)
    return
  }

  x.value -= event.deltaX
  y.value -= event.deltaY
  fitted = false
}

function onPointerDown(event: PointerEvent) {
  if (event.button !== 0)
    return
  if (!pointers.size)
    suppressClick = false
  const link = event.target instanceof Element && event.target.closest('a[href]')
  // Keep an ordinary anchor tap targeted at the anchor. Capture it only once it becomes a drag.
  if (!link)
    stage.value!.setPointerCapture(event.pointerId)
  pointers.set(event.pointerId, { x: event.clientX, y: event.clientY, dragging: false })
  if (pointers.size === 2) {
    suppressClick = true
    for (const [id, pointer] of pointers) {
      pointer.dragging = true
      stage.value!.setPointerCapture(id)
    }

    const [a, b] = [...pointers.values()]
    const bounds = stage.value!.getBoundingClientRect()
    pinch = {
      distance: Math.max(1, Math.hypot(a!.x - b!.x, a!.y - b!.y)),
      scale: scale.value,
      x: (a!.x + b!.x) / 2 - bounds.left,
      y: (a!.y + b!.y) / 2 - bounds.top,
    }
  }
}

function onPointerMove(event: PointerEvent) {
  const previous = pointers.get(event.pointerId)
  if (!previous)
    return
  const current = { x: event.clientX, y: event.clientY, dragging: previous.dragging }
  if (!current.dragging && Math.hypot(current.x - previous.x, current.y - previous.y) <= 5)
    return
  current.dragging = true
  suppressClick = true
  stage.value!.setPointerCapture(event.pointerId)
  event.preventDefault()
  pointers.set(event.pointerId, current)
  if (pinch && pointers.size === 2) {
    const [a, b] = [...pointers.values()]
    const bounds = stage.value!.getBoundingClientRect()
    const distance = Math.hypot(a!.x - b!.x, a!.y - b!.y)
    const px = (a!.x + b!.x) / 2 - bounds.left
    const py = (a!.y + b!.y) / 2 - bounds.top
    x.value += px - pinch.x
    y.value += py - pinch.y
    pinch.x = px
    pinch.y = py
    zoomAt(pinch.scale * distance / pinch.distance, px, py)
    return
  }

  x.value += current.x - previous.x
  y.value += current.y - previous.y
  fitted = false
}

function onPointerUp(event: PointerEvent) {
  pointers.delete(event.pointerId)
  if (pointers.size < 2)
    pinch = undefined
}

function onStageClick(event: MouseEvent) {
  if (suppressClick && event.detail > 0) {
    event.preventDefault()
    event.stopPropagation()
  }

  suppressClick = false
  if (!event.defaultPrevented)
    emit('openLink', event)
}

function onKey(event: KeyboardEvent) {
  if (event.key === '+' || event.key === '=')
    zoomBy(1.25)
  else if (event.key === '-')
    zoomBy(0.8)
  else if (event.key === '0')
    fit()
  else
    return
  event.preventDefault()
}

async function copySource() {
  await navigator.clipboard?.writeText(props.source)
  copied.value = true
  clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => copied.value = false, 1500)
}

/** Capture colors before awaiting font resources, including when the viewer closes meanwhile. */
function exportColors() {
  const probe = document.createElement('span')
  drawing.value!.append(probe)
  const colors = new Map<string, string>()
  const resolve = (tone: string) => {
    if (!colors.has(tone)) {
      probe.style.color = `var(--dg-${tone})`
      colors.set(tone, getComputedStyle(probe).color)
    }

    return colors.get(tone)!
  }

  try {
    resolve('canvas')
    for (const item of scene!.items) {
      for (const paint of item.kind === 'path' ? [item.fill, item.stroke] : [item.fill]) {
        if (paint && 'tone' in paint)
          resolve(paint.tone)
      }
    }
  }
  finally {
    probe.remove()
  }

  return (tone: string) => colors.get(tone)!
}

function save(blob: Blob, extension: string) {
  const link = document.createElement('a')
  link.href = URL.createObjectURL(blob)
  link.download = `${title.toLowerCase().replaceAll(' ', '-')}.${extension}`
  link.click()
  setTimeout(() => URL.revokeObjectURL(link.href), 1000)
}

async function download(format: 'svg' | 'png') {
  if (!scene || exporting.value)
    return
  exporting.value = true
  exportError.value = ''
  try {
    const blob = await diagramExport(props.source, format, exportColors())
    save(blob, format)
  }
  catch {
    exportError.value = 'Couldn’t export this diagram. Please try again.'
  }
  finally {
    exporting.value = false
  }
}

onMounted(async () => {
  if (!scene)
    return
  const svg = sceneSvg(scene)
  for (const link of svg.querySelectorAll('a[href]')) {
    const href = link.getAttribute('href')!
    if (artifactLink(href, window.location.origin))
      link.setAttribute('href', apiResourceUrl(href))
  }

  drawing.value!.append(svg)
  window.addEventListener('pointermove', onPointerMove, { passive: false })
  window.addEventListener('pointerup', onPointerUp)
  window.addEventListener('pointercancel', onPointerUp)
  await nextTick()
  fit()
  observer = new ResizeObserver(() => {
    if (fitted)
      fit()
  })
  observer.observe(stage.value!)
})

onBeforeUnmount(() => {
  observer?.disconnect()
  window.removeEventListener('pointermove', onPointerMove)
  window.removeEventListener('pointerup', onPointerUp)
  window.removeEventListener('pointercancel', onPointerUp)
  clearTimeout(copiedTimer)
})
</script>

<template>
  <Modal
    :title="title"
    workbench
    :return-focus="returnFocus"
    @close="emit('close')"
  >
    <div class="diagram-viewer flex min-h-0 flex-1 flex-col">
      <div class="flex flex-wrap items-center gap-1 border-b border-line px-4 py-2" role="toolbar" aria-label="Diagram view">
        <button
          :class="iconButton"
          aria-label="Zoom out"
          title="Zoom out"
          @click="zoomBy(0.8)"
        >
          <Icon :name="ZoomOut" />
        </button>
        <button
          class="min-w-14 rounded-lg px-2 py-1 text-xs font-semibold text-muted tabular-nums hover:bg-hover hover:text-ink"
          aria-label="Fit to view"
          title="Fit to view"
          @click="fit"
        >
          {{ percent }}
        </button>
        <button
          :class="iconButton"
          aria-label="Zoom in"
          title="Zoom in"
          @click="zoomBy(1.25)"
        >
          <Icon :name="ZoomIn" />
        </button>
        <button
          :class="iconButton"
          aria-label="Fit to view"
          title="Fit to view"
          @click="fit"
        >
          <Icon :name="Scan" />
        </button>
        <span class="flex-1" />
        <button
          :class="iconButton"
          :aria-label="copied ? 'Copied' : 'Copy source'"
          :title="copied ? 'Copied' : 'Copy source'"
          @click="copySource"
        >
          <Icon :name="copied ? Check : Copy" />
        </button>
        <button
          class="inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-semibold text-muted hover:bg-hover hover:text-ink"
          :disabled="!scene || exporting"
          @click="download('svg')"
        >
          <Icon :name="Download" :size="16" /> SVG
        </button>
        <button
          class="inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs font-semibold text-muted hover:bg-hover hover:text-ink"
          :disabled="!scene || exporting"
          @click="download('png')"
        >
          <Icon :name="Download" :size="16" /> PNG
        </button>
      </div>
      <p v-if="exporting" role="status" class="border-b border-line px-4 py-2 text-sm text-muted">
        Preparing export…
      </p>
      <p v-if="exportError" role="alert" class="border-b border-line px-4 py-2 text-sm text-danger">
        {{ exportError }}
      </p>
      <p v-if="openingLink" role="status" class="border-b border-line px-4 py-2 text-sm text-muted">
        Opening file…
      </p>
      <p v-if="linkError" role="alert" class="border-b border-line px-4 py-2 text-sm text-danger">
        {{ linkError }}
      </p>
      <div
        ref="stage"
        class="diagram-stage relative min-h-0 flex-1 touch-none overflow-hidden select-none"
        tabindex="0"
        aria-label="Diagram canvas. Drag to move, scroll or pinch to zoom."
        @wheel="onWheel"
        @pointerdown="onPointerDown"
        @click.capture="onStageClick"
        @dragstart.prevent
        @dblclick="fit"
        @keydown="onKey"
      >
        <div
          ref="drawing"
          class="absolute top-0 left-0 origin-top-left"
          :style="{ transform: `translate(${x}px, ${y}px) scale(${scale})` }"
        />
        <p v-if="!scene" class="p-6 text-sm text-muted">
          This diagram can’t be drawn.
        </p>
      </div>
    </div>
  </Modal>
</template>
