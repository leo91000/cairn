<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import type { DraftState } from '../attachments'
import { ref } from 'vue'
import { isImage, subtitle } from '../attachments'
import { X } from '../icons'
import AttachmentName from './AttachmentName.vue'
import AttachmentThumb from './AttachmentThumb.vue'
import AttachmentViewer from './AttachmentViewer.vue'
import FileBadge from './FileBadge.vue'
import Icon from './Icon.vue'

// A: the draft's attachments in a horizontal row above the message field. Images are 64 px squares,
// files 168 px cards. A': each item shows its own upload progress, failure or rejection.
const props = defineProps<{
  attachments: ChatAttachment[]
  /** Local object URLs for images picked on this device. */
  previews?: Record<string, string>
  states?: Record<string, DraftState>
  disabled?: boolean
}>()
const emit = defineEmits<{ remove: [id: string] }>()
const viewing = ref<number>()
const state = (attachment: ChatAttachment) => props.states?.[attachment.id]

function problem(attachment: ChatAttachment) {
  const status = state(attachment)?.status
  return status === 'failed' || status === 'rejected'
}

const square = (attachment: ChatAttachment) => isImage(attachment) && !problem(attachment)
const ring = (progress: number) => `${Math.max(0, Math.min(1, progress)) * 62.83} 62.83`

function label(attachment: ChatAttachment) {
  const current = state(attachment)
  if (current?.status === 'rejected')
    return `${attachment.name}: ${current.error}`
  if (current?.status === 'failed')
    return `${attachment.name}: upload failed, ${current.error}`
  return `Preview ${attachment.name}`
}
</script>

<template>
  <div class="min-w-0">
    <ul class="composer-tray m-0! flex list-none gap-2 overflow-x-auto p-0! pt-1.5! pr-1.5! pb-2! [scrollbar-width:thin]" aria-label="Attachments">
      <li
        v-for="(attachment, position) in attachments"
        :key="attachment.id"
        class="relative shrink-0"
        :data-state="state(attachment)?.status"
      >
        <button
          type="button"
          class="relative block overflow-hidden rounded-xl text-left focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          :class="square(attachment)
            ? 'size-16 bg-variant'
            : ['flex h-16 w-42 items-center gap-2.25 border px-2.5', problem(attachment) ? 'border-danger/45 bg-danger-surface' : 'border-line bg-surface']"
          :aria-label="label(attachment)"
          :title="attachment.name"
          @click="viewing = position"
        >
          <AttachmentThumb
            v-if="square(attachment)"
            :attachment="attachment"
            :local="previews?.[attachment.id]"
            eager
            decorative
          />
          <template v-else>
            <span v-if="isImage(attachment) && previews?.[attachment.id]" class="block h-11 w-9.5 shrink-0 overflow-hidden rounded-[7px]">
              <AttachmentThumb :attachment="attachment" :local="previews[attachment.id]" decorative />
            </span>
            <FileBadge v-else :attachment="attachment" />
            <span class="min-w-0 flex-1">
              <AttachmentName :name="attachment.name" class="text-xs font-semibold text-ink" />
              <span
                v-if="problem(attachment)"
                class="line-clamp-2 text-[10.5px] leading-tight text-danger"
              >{{ state(attachment)?.error }}</span>
              <span v-else class="block truncate text-[10.5px] text-muted">{{ subtitle(attachment, null, { details: false }) }}</span>
            </span>
          </template>
          <span
            v-if="state(attachment)?.status === 'uploading'"
            class="absolute inset-0 grid place-items-center rounded-xl bg-[#14141e73]"
            role="progressbar"
            :aria-label="`Uploading ${attachment.name}`"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-valuenow="state(attachment)?.progress === undefined ? undefined : Math.round(state(attachment)!.progress! * 100)"
          >
            <svg
              viewBox="0 0 26 26"
              class="size-6.5 -rotate-90"
              :class="state(attachment)?.progress === undefined ? 'animate-spin' : ''"
              aria-hidden="true"
            >
              <circle
                cx="13"
                cy="13"
                r="10"
                fill="none"
                stroke="#ffffff4d"
                stroke-width="3"
              />
              <circle
                cx="13"
                cy="13"
                r="10"
                fill="none"
                stroke="#fff"
                stroke-width="3"
                stroke-linecap="round"
                :stroke-dasharray="ring(state(attachment)?.progress ?? 0.25)"
                class="transition-[stroke-dasharray] duration-200"
              />
            </svg>
          </span>
        </button>
        <span v-if="problem(attachment)" class="sr-only" role="status">{{ label(attachment) }}</span>
        <button
          v-if="state(attachment)?.status !== 'uploading'"
          type="button"
          class="remove-attachment absolute -top-1.5 -right-1.5 z-2 grid size-5 place-items-center rounded-full border-2 border-surface bg-ink text-surface disabled:opacity-50"
          :aria-label="`Remove ${attachment.name}`"
          :title="`Remove ${attachment.name}`"
          :disabled="disabled"
          @click="emit('remove', attachment.id)"
        >
          <Icon :name="X" :size="11" />
        </button>
      </li>
    </ul>
    <AttachmentViewer
      v-if="viewing !== undefined"
      :attachments="attachments"
      :previews="previews"
      :initial="viewing"
      @close="viewing = undefined"
    />
  </div>
</template>

<style scoped>
/* A 36 px target around the 20 px badge. */
.remove-attachment::before {
  content: '';
  position: absolute;
  inset: -8px;
}
.remove-attachment:focus-visible {
  outline: 2px solid var(--color-accent);
  outline-offset: 2px;
}
</style>
