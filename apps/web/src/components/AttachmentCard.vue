<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import { computed, ref } from 'vue'
import { useAttachmentMedia } from '../attachment-media'
import {
  detail,
  documentPreview,
  size,
  subtitle,
} from '../attachments'
import AttachmentActions from './AttachmentActions.vue'
import AttachmentName from './AttachmentName.vue'
import FileBadge from './FileBadge.vue'

// B3 "document" card (PDF first page or text excerpt) and B2 compact card for other files.
const props = defineProps<{ attachment: ChatAttachment, document?: boolean }>()
defineEmits<{ open: [] }>()
const area = ref<HTMLElement>()
const kind = computed(() => documentPreview(props.attachment))
const media = useAttachmentMedia(() => props.document ? props.attachment : undefined, { target: area, preview: kind.value === 'page', summary: true })
const pageFailed = ref(false)
const documentLine = computed(() => [detail(props.attachment, media.summary.value), size(props.attachment.size)].filter(Boolean).join(' · '))
const actions = 'absolute top-1.5 right-1.5 grid size-7 place-items-center rounded-full border border-line bg-surface/95 text-muted shadow-sm opacity-0 transition-opacity hover:text-ink group-hover:opacity-100 group-focus-within:opacity-100 aria-expanded:opacity-100 pointer-coarse:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-accent'
</script>

<template>
  <li v-if="document" class="group relative w-28 shrink-0 overflow-hidden rounded-[14px] border border-line bg-surface">
    <button
      type="button"
      class="block w-full text-left focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
      :aria-label="`Preview ${attachment.name}`"
      :title="attachment.name"
      @click="$emit('open')"
    >
      <span ref="area" class="relative block h-23 overflow-hidden bg-variant">
        <img
          v-if="kind === 'page' && media.preview.value && !pageFailed"
          :src="media.preview.value"
          alt=""
          class="absolute inset-x-3.5 top-2.5 h-[calc(100%-2px)] w-[calc(100%-28px)] bg-white object-cover object-top shadow-[0_1px_3px_#0000001f]"
          @error="pageFailed = true"
        >
        <span
          v-else-if="kind === 'excerpt' && media.summary.value?.excerpt"
          class="block h-full overflow-hidden p-2 font-mono text-[7.5px] leading-[1.35] whitespace-pre text-muted"
          aria-hidden="true"
        >{{ media.summary.value.excerpt.slice(0, 400) }}</span>
        <span v-else class="grid size-full place-items-center">
          <FileBadge :attachment="attachment" />
        </span>
        <FileBadge :attachment="attachment" chip class="absolute bottom-1.5 left-1.5" />
      </span>
      <span class="block px-2 pt-1.5 pb-2">
        <AttachmentName :name="attachment.name" class="text-[11px] font-semibold text-ink" />
        <span class="block truncate text-[10.5px] text-muted">{{ documentLine }}</span>
      </span>
    </button>
    <AttachmentActions :attachment="attachment" :trigger-class="actions" />
  </li>
  <li v-else class="group relative w-55 max-w-full shrink-0">
    <button
      type="button"
      class="flex h-14.5 w-full items-center gap-2.5 rounded-xl border border-line bg-surface px-2.5 pr-9 text-left hover:bg-hover/60 focus-visible:outline-2 focus-visible:outline-accent"
      :aria-label="`Preview ${attachment.name}`"
      :title="attachment.name"
      @click="$emit('open')"
    >
      <FileBadge :attachment="attachment" />
      <span class="min-w-0 flex-1">
        <AttachmentName :name="attachment.name" class="text-xs font-semibold text-ink" />
        <span class="block truncate text-[10.5px] text-muted">{{ subtitle(attachment) }}</span>
      </span>
    </button>
    <AttachmentActions :attachment="attachment" :trigger-class="[actions, 'top-1/2! -translate-y-1/2']" />
  </li>
</template>
