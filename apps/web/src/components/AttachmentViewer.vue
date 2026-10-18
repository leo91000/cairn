<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { notify, state } from '../api'
import {
  attachmentUrl,
  canShareFiles,
  downloadAttachment,
  useAttachmentMedia,
  useAttachmentShare,
} from '../attachment-media'
import {
  detail,
  documentPreview,
  isImage,
  size,
  typeLabel,
} from '../attachments'
import {
  ChevronLeft,
  ChevronRight,
  Download,
  Maximize2,
  Minimize2,
  Share2,
  X,
} from '../icons'
import AttachmentThumb from './AttachmentThumb.vue'
import FileBadge from './FileBadge.vue'
import Icon from './Icon.vue'

// C: a full-screen dark viewer that pages through every attachment of a message (or the draft).
const props = defineProps<{
  attachments: ChatAttachment[]
  initial: number
  /** Local object URLs for draft images that are not uploaded yet. */
  previews?: Record<string, string>
}>()
const emit = defineEmits<{ close: [] }>()
const dialog = ref<HTMLDialogElement>()
const strip = ref<HTMLElement>()
const index = ref(Math.min(Math.max(props.initial, 0), Math.max(props.attachments.length - 1, 0)))
const current = computed(() => props.attachments[index.value])
const local = computed(() => current.value ? props.previews?.[current.value.id] : undefined)
const uploaded = computed(() => !!current.value?.chatId)
const image = computed(() => !!current.value && isImage(current.value))
const preview = computed(() => current.value ? documentPreview(current.value) : null)
const page = useAttachmentMedia(() => preview.value === 'page' ? current.value : undefined, { preview: true, summary: true })
const text = useAttachmentMedia(() => preview.value === 'excerpt' ? current.value : undefined, { summary: true })
const dimensions = ref<{ width: number, height: number }>()
const broken = ref(false)
const share = canShareFiles()
const sharing = useAttachmentShare(() => current.value)
const originalImage = ref<string>()
watch(sharing.file, (file, _, cleanup) => {
  originalImage.value = undefined
  if (!file || !image.value)
    return
  const url = URL.createObjectURL(file)
  originalImage.value = url
  cleanup(() => URL.revokeObjectURL(url))
})
const zoom = ref(false)
const previous = document.activeElement as HTMLElement | null
const imageSource = computed(() => {
  if (local.value)
    return local.value
  return originalImage.value
})
watch(imageSource, () => {
  dimensions.value = undefined
  broken.value = false
  zoom.value = false
})
const summary = computed(() => preview.value === 'page' ? page.summary.value : text.summary.value)
const caption = computed(() => {
  const attachment = current.value
  if (!attachment)
    return ''
  return [
    typeLabel(attachment.name),
    size(attachment.size),
    dimensions.value ? `${dimensions.value.width} × ${dimensions.value.height}` : '',
  ].filter(Boolean).join(' · ')
})

watch(index, async () => {
  dimensions.value = undefined
  broken.value = false
  zoom.value = false
  await nextTick()
  strip.value?.querySelector<HTMLElement>('[aria-current="true"]')?.scrollIntoView?.({ block: 'nearest', inline: 'center' })
})

watch(() => [current.value, sharing.file.value], () => {
  if ((share || image.value) && uploaded.value && !local.value)
    void prepareShare()
}, { immediate: true })
watch(() => props.attachments.length, (length) => {
  if (!length)
    emit('close')
  else if (index.value >= length)
    index.value = length - 1
})

function go(step: number) {
  const count = props.attachments.length
  if (count > 1)
    index.value = (index.value + step + count) % count
}

function loaded(event: Event) {
  const element = event.target as HTMLImageElement
  // A previous image can finish loading after the viewer moved on; only images report a size.
  if (!image.value || element.alt !== current.value?.name)
    return
  if (element.naturalWidth && element.naturalHeight)
    dimensions.value = { width: element.naturalWidth, height: element.naturalHeight }
}

function keydown(event: KeyboardEvent) {
  if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey)
    return
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    event.preventDefault()
    go(event.key === 'ArrowLeft' ? -1 : 1)
  }
}

let touch: { x: number, y: number } | undefined

function touchStart(event: TouchEvent) {
  touch = !zoom.value && event.touches.length === 1 ? { x: event.touches[0].clientX, y: event.touches[0].clientY } : undefined
}

function touchEnd(event: TouchEvent) {
  const start = touch
  touch = undefined
  const end = event.changedTouches[0]
  if (!start || !end)
    return
  const dx = end.clientX - start.x
  if (Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(end.clientY - start.y) * 1.5)
    go(dx < 0 ? 1 : -1)
}

async function shareCurrent() {
  try {
    await sharing.share()
  }
  catch (error) { notify((error as Error).message || 'Unable to share this file.') }
}

async function prepareShare() {
  try {
    await sharing.prepare()
  }
  catch (error) { notify((error as Error).message || 'Unable to load this file.') }
}

async function downloadCurrent() {
  if (!current.value)
    return
  try {
    await downloadAttachment(current.value, sharing.file.value)
  }
  catch (error) { notify((error as Error).message || 'Unable to download this file.') }
}

onMounted(async () => {
  await nextTick()
  dialog.value?.showModal()
})
onBeforeUnmount(() => {
  dialog.value?.close()
  if (previous?.isConnected)
    previous.focus()
})
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="attachment-viewer fixed inset-0 m-0 h-dvh max-h-none w-screen max-w-none border-0 bg-[#0d0d12] p-0 text-white backdrop:bg-black/60"
      aria-label="Attachment viewer"
      @cancel.prevent="emit('close')"
      @keydown="keydown"
    >
      <div v-if="current" class="flex h-full min-h-0 flex-col pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]">
        <header class="flex shrink-0 items-center gap-2.5 px-3 py-2">
          <button
            type="button"
            class="viewer-button"
            aria-label="Close viewer"
            @click="emit('close')"
          >
            <Icon :name="X" :size="18" />
          </button>
          <div class="min-w-0 flex-1">
            <h2 class="m-0! truncate text-sm! font-semibold text-white" :title="current.name">
              {{ current.name }}
            </h2>
            <p class="m-0! truncate text-xs text-[#9d9bab]">
              {{ caption }}
            </p>
          </div>
          <button
            v-if="image && imageSource && !broken"
            type="button"
            class="viewer-button"
            :aria-label="zoom ? 'Fit image' : 'Actual size'"
            :title="zoom ? 'Fit image' : 'Actual size'"
            :aria-pressed="zoom"
            @click="zoom = !zoom"
          >
            <Icon :name="zoom ? Minimize2 : Maximize2" :size="18" />
          </button>
          <a
            v-if="uploaded"
            class="viewer-button"
            :href="attachmentUrl(current)"
            :download="current.name"
            :aria-label="`Download ${current.name}`"
            title="Download"
            @click.prevent="downloadCurrent"
          ><Icon :name="Download" :size="18" /></a>
          <button
            v-if="uploaded && share"
            type="button"
            class="viewer-button"
            :aria-label="`Share ${current.name}`"
            :title="sharing.loading.value ? 'Preparing file…' : sharing.file.value ? 'Share' : 'Prepare to share'"
            :disabled="sharing.loading.value"
            @click="sharing.file.value ? shareCurrent() : prepareShare()"
          >
            <Icon :name="Share2" :size="18" />
          </button>
        </header>
        <p
          v-if="state.toast || state.connectionLost"
          class="m-0! shrink-0 border-y border-white/10 px-4 py-2 text-xs text-[#c9c7d6]"
          role="status"
          aria-live="polite"
        >
          {{ state.toast || 'Connection lost · reconnecting…' }}
        </p>
        <div
          class="relative flex min-h-0 flex-1 items-center justify-center px-3 sm:px-16"
          @touchstart.passive="touchStart"
          @touchend="touchEnd"
        >
          <div
            v-if="image && imageSource && !broken"
            class="h-full w-full overflow-auto"
            aria-label="Image preview"
          >
            <div class="flex min-h-full" :class="zoom ? 'w-max min-w-full' : 'h-full items-center justify-center'">
              <img
                :key="current.id"
                :src="imageSource"
                :alt="current.name"
                :class="zoom ? 'm-auto max-w-none shrink-0' : 'max-h-full max-w-full object-contain'"
                class="block select-none"
                @load="loaded"
                @error="broken = true"
              >
            </div>
          </div>
          <img
            v-else-if="preview === 'page' && page.preview.value"
            :key="`page:${current.id}`"
            :src="page.preview.value"
            :alt="`First page of ${current.name}`"
            class="block max-h-full max-w-full bg-white object-contain shadow-2xl"
          >
          <pre
            v-else-if="preview === 'excerpt' && text.summary.value?.excerpt"
            class="m-0! max-h-full w-full max-w-3xl overflow-auto rounded-xl bg-white/6 p-5 font-mono text-xs leading-relaxed whitespace-pre-wrap text-[#e7e6ef] wrap-anywhere"
            tabindex="0"
            :aria-label="`Beginning of ${current.name}`"
          >{{ text.summary.value.excerpt }}</pre>
          <div v-else class="flex flex-col items-center gap-4 text-center">
            <FileBadge :attachment="current" large />
            <p class="m-0! max-w-sm text-sm text-[#c9c7d6] wrap-anywhere">
              {{ current.name }}
            </p>
            <p v-if="(image && sharing.loading.value && !broken) || (preview === 'page' && !page.previewFailed.value && uploaded) || (preview === 'excerpt' && summary === undefined && uploaded)" class="m-0! text-xs text-[#9d9bab]" role="status">
              Loading preview…
            </p>
            <template v-else-if="uploaded">
              <p class="m-0! text-xs text-[#9d9bab]">
                No preview available{{ detail(current, summary) ? ` · ${detail(current, summary)}` : '' }}
              </p>
              <a
                class="inline-flex min-h-10 items-center gap-2 rounded-full bg-white px-4 text-sm font-semibold text-[#0d0d12]"
                :href="attachmentUrl(current)"
                :download="current.name"
                @click.prevent="downloadCurrent"
              ><Icon :name="Download" :size="16" />Download</a>
            </template>
            <p v-else class="m-0! text-xs text-[#9d9bab]">
              Uploaded when you send the message
            </p>
          </div>
          <template v-if="attachments.length > 1">
            <button
              type="button"
              class="viewer-button viewer-nav left-3"
              aria-label="Previous attachment"
              title="Previous (←)"
              @click="go(-1)"
            >
              <Icon :name="ChevronLeft" :size="20" />
            </button>
            <button
              type="button"
              class="viewer-button viewer-nav right-3"
              aria-label="Next attachment"
              title="Next (→)"
              @click="go(1)"
            >
              <Icon :name="ChevronRight" :size="20" />
            </button>
          </template>
        </div>
        <p class="m-0! shrink-0 pt-2 text-center text-2xs text-[#9d9bab]" aria-live="polite">
          {{ index + 1 }} of {{ attachments.length }}<span v-if="detail(current, summary)"> · {{ detail(current, summary) }}</span>
        </p>
        <ul
          v-if="attachments.length > 1"
          ref="strip"
          class="m-0! flex shrink-0 list-none justify-center-safe gap-1.5 overflow-x-auto px-3 pt-2 pb-4"
          aria-label="Attachments"
        >
          <li v-for="(attachment, position) in attachments" :key="attachment.id" class="shrink-0 p-0.5">
            <button
              type="button"
              class="block size-10 overflow-hidden rounded-lg transition-opacity focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-white"
              :class="position === index ? 'opacity-100 outline-2 outline-offset-1 outline-white' : 'opacity-55 hover:opacity-90'"
              :aria-label="`Show ${attachment.name}`"
              :aria-current="position === index ? 'true' : undefined"
              @click="index = position"
            >
              <AttachmentThumb :attachment="attachment" :local="previews?.[attachment.id]" decorative />
            </button>
          </li>
        </ul>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.viewer-button {
  display: grid;
  width: 40px;
  height: 40px;
  flex-shrink: 0;
  place-items: center;
  border-radius: 9999px;
  background: rgb(255 255 255 / .1);
  color: #fff;
  transition: background-color .15s;
}
.viewer-button:hover { background: rgb(255 255 255 / .18); }
.viewer-button:focus-visible { outline: 2px solid #fff; outline-offset: 2px; }
.viewer-nav {
  position: absolute;
  top: 50%;
  transform: translateY(-50%);
}
.attachment-viewer[open] {
  display: block;
}
@media (prefers-reduced-motion: no-preference) {
  .attachment-viewer[open] { animation: viewer-in .16s ease-out; }
}
@keyframes viewer-in {
  from { opacity: 0; }
}
</style>
