<script setup lang="ts">
import type { PDFDocumentProxy, RenderTask } from 'pdfjs-dist'
import { getDocument, GlobalWorkerOptions, version } from 'pdfjs-dist'
import workerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url'
import {
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { ArrowLeft, ChevronRight } from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'

const props = defineProps<{ url: string, title: string }>()
GlobalWorkerOptions.workerSrc = workerUrl
const canvas = ref<HTMLCanvasElement>()
const page = ref(1)
const pages = ref(0)
const scale = ref(1)
const busy = ref(true)
const error = ref('')
let document: PDFDocumentProxy | undefined
let render: RenderTask | undefined
let disposed = false
let generation = 0
const base = `/pdfjs/${version}/`
const loading = getDocument({
  url: props.url,
  withCredentials: true,
  maxImageSize: 16_000_000,
  canvasMaxAreaInBytes: 64_000_000,
  cMapUrl: `${base}cmaps/`,
  cMapPacked: true,
  standardFontDataUrl: `${base}standard_fonts/`,
  wasmUrl: `${base}wasm/`,
})

async function draw() {
  const turn = ++generation
  render?.cancel()
  busy.value = true
  error.value = ''
  try {
    const pdfPage = await document?.getPage(page.value)
    await nextTick()
    if (!pdfPage || disposed || turn !== generation || !canvas.value)
      return
    const viewport = pdfPage.getViewport({ scale: scale.value * 1.5 })
    canvas.value.width = viewport.width
    canvas.value.height = viewport.height
    render = pdfPage.render({ canvas: canvas.value, viewport })
    await render.promise
  }
  catch (e) {
    if (!disposed && turn === generation && (e as Error).name !== 'RenderingCancelledException')
      error.value = 'This PDF cannot be previewed. Download the original to open it.'
  }
  finally {
    if (turn === generation)
      busy.value = false
  }
}

onMounted(async () => {
  try {
    document = await loading.promise
    if (disposed)
      return
    pages.value = document.numPages
    await draw()
  }
  catch {
    if (!disposed) {
      error.value = 'This PDF cannot be previewed. It may require a password. Download the original to open it.'
      busy.value = false
    }
  }
})
watch([page, scale], draw)
onBeforeUnmount(() => {
  disposed = true
  render?.cancel()
  void loading.destroy()
})
</script>

<template>
  <div>
    <div class="sticky top-0 z-1 mb-4 flex items-center justify-center gap-3 rounded-lg bg-raised/95 p-2 text-xs backdrop-blur">
      <button
        :class="iconButton"
        aria-label="Previous PDF page"
        :disabled="page <= 1 || busy"
        @click="page--"
      >
        <Icon :name="ArrowLeft" :size="16" />
      </button>
      <span role="status">{{ pages ? `Page ${page} of ${pages}` : 'Loading PDF…' }}</span>
      <button
        :class="iconButton"
        aria-label="Next PDF page"
        :disabled="page >= pages || busy"
        @click="page++"
      >
        <Icon :name="ChevronRight" :size="16" />
      </button>
      <button
        class="rounded px-2 py-1 hover:bg-hover"
        :disabled="busy"
        aria-label="Change PDF zoom"
        @click="scale = scale === 1 ? 1.5 : scale === 1.5 ? 2 : 1"
      >
        {{ scale * 100 }}%
      </button>
    </div>
    <p v-if="error" role="alert" class="text-sm text-muted">
      {{ error }}
    </p>
    <canvas
      v-show="!error"
      ref="canvas"
      :aria-label="`${title}, page ${page}`"
      role="img"
      class="mx-auto block bg-white shadow-sm"
      :class="scale === 1 ? 'max-w-full' : 'max-w-none'"
      :style="{ width: canvas ? `${canvas.width / 1.5}px` : undefined }"
      :aria-busy="busy"
    />
  </div>
</template>
