<script setup lang="ts">
import type { ChatAttachment } from '../../shared/chats'
import { ref } from 'vue'
import { FileText, X } from '../icons'
import Icon from './Icon.vue'
import Modal from './Modal.vue'

defineProps<{
  attachments: ChatAttachment[]
  previews?: Record<string, string>
  removable?: boolean
  disabled?: boolean
}>()
defineEmits<{ remove: [id: string] }>()
const expanded = ref<{ name: string, url: string }>()

function url(attachment: ChatAttachment) {
  return `/api/chats/${encodeURIComponent(attachment.chatId)}/attachments/${encodeURIComponent(attachment.id)}`
}

function size(bytes: number) {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${Math.ceil(bytes / 1024)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`
}
</script>

<template>
  <div class="min-w-0">
    <ul class="m-0! flex gap-2 p-0!" :class="removable ? 'overflow-x-auto' : 'flex-wrap'" aria-label="Attachments">
      <li
        v-for="attachment in attachments"
        :key="attachment.id"
        class="relative flex min-w-0 shrink-0 overflow-hidden rounded-xl border border-line bg-surface"
        :class="removable ? 'h-20 w-52 items-center phone:w-44' : 'w-36 flex-col phone:w-28'"
      >
        <button
          v-if="attachment.kind === 'image'"
          type="button"
          class="group block shrink-0 overflow-hidden bg-soft"
          :class="removable ? 'ml-1.5 size-16 rounded-lg' : 'h-20'"
          :aria-label="`Preview ${attachment.name}`"
          @click="expanded = { name: attachment.name, url: previews?.[attachment.id] || url(attachment) }"
        >
          <img :src="previews?.[attachment.id] || url(attachment)" :alt="attachment.name" class="size-full object-cover transition-transform group-hover:scale-105">
        </button>
        <a
          v-else-if="attachment.chatId"
          :href="url(attachment)"
          :download="attachment.name"
          class="flex h-12 items-center px-3 text-accent"
          :aria-label="`Download ${attachment.name}`"
        ><Icon :name="FileText" :size="24" /></a>
        <span v-else class="flex h-12 items-center px-3 text-accent"><Icon :name="FileText" :size="24" /></span>
        <div class="min-w-0 px-2.5 py-2" :class="{ 'pt-6': removable }">
          <p class="m-0! truncate text-[11px] font-semibold" :title="attachment.name">
            {{ attachment.name }}
          </p>
          <span class="text-[10px] text-muted">{{ size(attachment.size) }}</span>
        </div>
        <button
          v-if="removable"
          type="button"
          class="absolute top-1 right-1 grid size-7 place-items-center rounded-full border border-line bg-surface/95 text-muted shadow-sm hover:text-danger disabled:opacity-50"
          :aria-label="`Remove ${attachment.name}`"
          :disabled="disabled"
          @click="$emit('remove', attachment.id)"
        >
          <Icon :name="X" :size="14" />
        </button>
      </li>
    </ul>
    <Modal v-if="expanded" :title="expanded.name" @close="expanded = undefined">
      <div class="flex min-h-0 items-center justify-center bg-soft p-3">
        <img :src="expanded.url" :alt="expanded.name" class="max-h-[70dvh] max-w-full object-contain">
      </div>
    </Modal>
  </div>
</template>
