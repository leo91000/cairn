<script setup lang="ts">
import type { Deliverable } from '../../shared/artifacts'
import {
  computed,
  defineAsyncComponent,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { artifactUrl, fileSize, latestArtifacts } from '../../shared/artifacts'
import { highlight } from '../highlight'
import {
  ArrowDown,
  ArrowLeft,
  ChevronRight,
  FileText,
  Link,
  Maximize2,
  Minimize2,
  X,
} from '../icons'
import { iconButton } from '../ui'
import ArtifactGallery from './ArtifactGallery.vue'
import ArtifactSharing from './ArtifactSharing.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ items: Deliverable[], initial?: string }>()
const emit = defineEmits<{ close: [] }>()
const ArtifactPdf = defineAsyncComponent(() => import('./ArtifactPdf.vue'))
const dialog = ref<HTMLDialogElement>()
const selected = ref(props.initial ?? '')
const current = computed(() => props.items.find(item => item.id === selected.value))
const latest = computed(() => latestArtifacts(props.items))
const versions = computed(() => props.items.filter(item => item.key === current.value?.key && item.runId === current.value?.runId).sort((a, b) => b.version - a.version))
const previousVersion = computed(() => versions.value.find(item => item.version === (current.value?.version ?? 0) - 1 && item.kind === 'image'))
const compare = ref(false)
const split = ref(50)
const zoom = ref(false)
const full = ref(false)
const sharing = ref(false)
const width = ref(900)
const content = ref('')
const error = ref('')
const loading = ref(false)
const previousFocus = document.activeElement as HTMLElement | null
const language = computed(() => ({
  rs: 'rust',
  js: 'javascript',
  ts: 'typescript',
  tsx: 'typescript',
  py: 'python',
  sh: 'bash',
  yml: 'yaml',
})[current.value?.name.split('.').at(-1) ?? ''] ?? current.value?.name.split('.').at(-1))
const html = computed(() => highlight(content.value, language.value))
watch(() => current.value?.id, async (_, __, cleanup) => {
  const item = current.value
  const controller = new AbortController()
  cleanup(() => controller.abort())
  content.value = ''
  error.value = ''
  zoom.value = false
  compare.value = false
  loading.value = false
  if (!item || !['markdown', 'code'].includes(item.kind))
    return
  if (item.size > 512 * 1024) {
    error.value = 'This document is too large for an inline preview. Download the original to read it.'
    return
  }

  loading.value = true
  try {
    const response = await fetch(artifactUrl(item), { signal: controller.signal })
    if (!response.ok)
      throw new Error('Preview unavailable. Try again or download the original.')
    const text = await response.text()
    if (!controller.signal.aborted)
      content.value = text
  }
  catch (e) {
    if (!controller.signal.aborted)
      error.value = (e as Error).message
  }
  finally {
    if (!controller.signal.aborted)
      loading.value = false
  }
}, { immediate: true })

function navigate(direction: number) {
  const index = latest.value.findIndex(item => item.key === current.value?.key)
  selected.value = latest.value[(index + direction + latest.value.length) % latest.value.length]?.id ?? ''
}

function resize(event: PointerEvent) {
  const handle = event.currentTarget as HTMLElement
  handle.setPointerCapture(event.pointerId)
  handle.onpointermove = e => width.value = Math.max(480, Math.min(window.innerWidth - 80, window.innerWidth - e.clientX))
  handle.onpointerup = handle.onpointercancel = () => handle.onpointermove = null
}

function resizeKeyboard(delta: number) {
  width.value = Math.max(480, Math.min(window.innerWidth - 80, width.value + delta))
}

onMounted(() => dialog.value?.showModal())
onBeforeUnmount(() => {
  dialog.value?.close()
  previousFocus?.focus()
})
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="artifact-viewer fixed inset-y-0 right-0 left-auto m-0 h-dvh max-h-dvh max-w-full border-0 border-l border-line bg-raised p-0 text-ink shadow-2xl backdrop:bg-black/35 backdrop:backdrop-blur-sm phone:w-full!"
      :style="{ width: full ? '100vw' : `${width}px` }"
      aria-label="Deliverables viewer"
      @cancel.prevent="emit('close')"
    >
      <div class="flex h-full min-h-0 flex-col">
        <div
          v-if="!full"
          role="separator"
          aria-label="Resize viewer"
          aria-orientation="vertical"
          tabindex="0"
          class="absolute inset-y-0 left-0 z-10 w-1 cursor-ew-resize touch-none hover:bg-accent/40 phone:hidden"
          @pointerdown="resize"
          @keydown.left.prevent="resizeKeyboard(40)"
          @keydown.right.prevent="resizeKeyboard(-40)"
        />
        <header class="flex min-w-0 shrink-0 items-center gap-3 border-b border-line px-5 py-4 phone:px-3">
          <button
            v-if="current"
            :class="iconButton"
            aria-label="All deliverables"
            @click="selected = ''"
          >
            <Icon :name="ArrowLeft" :size="18" />
          </button>
          <div class="min-w-0 flex-1">
            <h2 class="m-0! truncate text-base!">
              {{ current?.title ?? 'Files' }}
            </h2><p class="m-0! mt-1! truncate text-xs text-muted">
              {{ current ? `${current.name} · ${fileSize(current.size)}` : `${latest.length} deliverables` }}
            </p>
          </div>
          <button
            v-if="current"
            :class="iconButton"
            aria-label="Share file"
            :aria-expanded="sharing"
            @click="sharing = !sharing"
          >
            <Icon :name="Link" :size="18" />
          </button>
          <a
            v-if="current"
            :href="artifactUrl(current, 'download')"
            :download="current.name"
            :class="iconButton"
            aria-label="Download original"
          ><Icon :name="ArrowDown" :size="19" /></a>
          <button
            class="phone:hidden"
            :class="[iconButton]"
            :aria-label="full ? 'Exit fullscreen' : 'Fullscreen preview'"
            @click="full = !full"
          >
            <Icon :name="full ? Minimize2 : Maximize2" :size="18" />
          </button>
          <button :class="iconButton" aria-label="Close viewer" @click="emit('close')">
            <Icon :name="X" :size="20" />
          </button>
        </header>
        <ArtifactSharing v-if="current && sharing" :key="current.id" :item="current" />
        <div v-if="current" class="flex shrink-0 flex-wrap items-center gap-2 border-b border-line px-5 py-2 phone:px-3">
          <VirtualSelect
            v-if="versions.length > 1"
            v-model="selected"
            class="w-36"
            label="Version"
            hide-label
            compact
            :options="versions.map(item => ({ value: item.id, label: `Version ${item.version}` }))"
          />
          <span v-else class="text-xs text-muted">Version {{ current.version }}</span>
          <button
            v-if="current.kind === 'image'"
            class="rounded-md px-3 py-2 text-xs hover:bg-hover"
            :aria-pressed="zoom"
            @click="zoom = !zoom; compare = false"
          >
            {{ zoom ? 'Fit image' : 'Actual size' }}
          </button>
          <button
            v-if="previousVersion"
            class="rounded-md px-3 py-2 text-xs hover:bg-hover"
            :class="{ 'bg-hover text-accent': compare }"
            :aria-pressed="compare"
            @click="compare = !compare; zoom = false"
          >
            Compare
          </button>
          <div v-if="latest.length > 1" class="ml-auto flex items-center gap-1">
            <button :class="iconButton" aria-label="Previous file" @click="navigate(-1)">
              <Icon :name="ArrowLeft" :size="16" />
            </button><button :class="iconButton" aria-label="Next file" @click="navigate(1)">
              <Icon :name="ChevronRight" :size="16" />
            </button>
          </div>
        </div>
        <div class="min-h-0 flex-1 overflow-auto overscroll-contain p-6 phone:p-3" :class="current && ['image', 'video', 'audio', 'pdf'].includes(current.kind) ? 'bg-inset' : ''">
          <ArtifactGallery v-if="!current" :items="latest" @open="selected = $event.id" />
          <template v-else>
            <p v-if="loading" role="status" class="text-sm text-muted">
              Loading document…
            </p>
            <p v-else-if="error" role="alert" class="text-sm text-muted">
              {{ error }}
            </p>
            <div v-else-if="compare && previousVersion" class="mx-auto max-w-full">
              <div class="relative overflow-hidden rounded-lg bg-surface">
                <img :src="artifactUrl(current)" :alt="`Version ${current.version}`" class="block h-auto w-full">
                <img
                  :src="artifactUrl(previousVersion)"
                  :alt="`Version ${previousVersion.version}`"
                  class="absolute inset-0 size-full object-contain"
                  :style="{ clipPath: `inset(0 ${100 - split}% 0 0)` }"
                >
                <span class="pointer-events-none absolute inset-y-0 w-0.5 bg-white shadow" :style="{ left: `${split}%` }" />
              </div>
              <label class="mt-4! flex! flex-row! items-center gap-3 text-xs"><span>v{{ previousVersion.version }}</span><input
                v-model.number="split"
                type="range"
                min="0"
                max="100"
                aria-label="Comparison position"
                class="min-w-0 flex-1 accent-accent"
              ><span>v{{ current.version }}</span></label>
            </div>
            <div v-else-if="current.kind === 'image'" class="flex min-h-full" :class="zoom ? 'w-max' : 'items-center justify-center'">
              <img :src="artifactUrl(current)" :alt="current.title" :class="zoom ? 'max-w-none' : 'max-h-[calc(100dvh-190px)] max-w-full object-contain'">
            </div>
            <div v-else-if="current.kind === 'video' || current.kind === 'audio'" class="flex min-h-full items-center justify-center">
              <video
                v-if="current.kind === 'video'"
                :key="current.id"
                :src="artifactUrl(current)"
                :poster="current.previewStatus === 'ready' ? artifactUrl(current, 'preview') : undefined"
                controls
                playsinline
                preload="metadata"
                :aria-label="current.title"
                class="max-h-[calc(100dvh-190px)] w-full rounded-lg"
                @error="error = 'Your browser cannot play this format. Download the original to open it.'"
              />
              <audio
                v-else
                :key="`audio:${current.id}`"
                :src="artifactUrl(current)"
                controls
                preload="metadata"
                :aria-label="current.title"
                class="w-full"
                @error="error = 'Your browser cannot play this format. Download the original to open it.'"
              />
            </div>
            <ArtifactPdf
              v-else-if="current.kind === 'pdf'"
              :key="current.id"
              :url="artifactUrl(current)"
              :title="current.title"
            />
            <Markdown v-else-if="current.kind === 'markdown'" :content="content" />
            <pre v-else-if="current.kind === 'code'" class="m-0 overflow-visible whitespace-pre-wrap break-words text-xs leading-relaxed"><code v-html="html" /></pre>
            <div v-else class="flex min-h-full flex-col items-center justify-center gap-4 text-center">
              <Icon :name="FileText" :size="44" class="text-accent" /><p class="text-sm text-muted">
                This file is ready to download.
              </p><a :href="artifactUrl(current, 'download')" :download="current.name" class="rounded-lg bg-accent px-4 py-2 text-sm text-white">Download {{ current.name }}</a>
            </div>
          </template>
        </div>
      </div>
    </dialog>
  </Teleport>
</template>
