<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import { computed, ref } from 'vue'
import { arrange, mosaic } from '../attachments'
import AttachmentActions from './AttachmentActions.vue'
import AttachmentCard from './AttachmentCard.vue'
import AttachmentThumb from './AttachmentThumb.vue'
import AttachmentViewer from './AttachmentViewer.vue'

// B2/B3: a sent message's attachments, drawn above its bubble and right-aligned: an image mosaic,
// document cards for previewable files, then compact cards. Every item opens the viewer (C).
const props = defineProps<{ attachments: ChatAttachment[] }>()
const groups = computed(() => arrange(props.attachments))
const layout = computed(() => mosaic(groups.value.images.length))
const tiles = computed(() => groups.value.images.slice(0, layout.value.shown))
// The viewer pages in the order the items are drawn.
const ordered = computed(() => [...groups.value.images, ...groups.value.documents, ...groups.value.files])
const viewing = ref<number>()

function open(attachment: ChatAttachment) {
  viewing.value = Math.max(0, ordered.value.findIndex(item => item.id === attachment.id))
}

const actions = 'absolute top-1.5 right-1.5 z-1 grid size-7 place-items-center rounded-full bg-black/55 text-white opacity-0 backdrop-blur-sm transition-opacity hover:bg-black/70 group-hover:opacity-100 group-focus-within:opacity-100 aria-expanded:opacity-100 pointer-coarse:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-white'
</script>

<template>
  <div class="message-attachments flex max-w-full flex-col items-end gap-1.5" role="group" aria-label="Attachments">
    <ul
      v-if="tiles.length"
      class="m-0! list-none overflow-hidden rounded-2xl p-0!"
      :class="{
        'max-w-60': layout.layout === 'single',
        'grid w-60 max-w-full grid-cols-2 gap-[3px]': layout.layout === 'pair' || layout.layout === 'grid',
        'grid w-60 max-w-full grid-cols-[1.4fr_1fr] grid-rows-[80px_80px] gap-[3px]': layout.layout === 'triple',
      }"
      data-testid="attachment-mosaic"
    >
      <li
        v-for="(attachment, position) in tiles"
        :key="attachment.id"
        class="group relative min-w-0"
        :class="{
          'h-28': layout.layout === 'pair' || layout.layout === 'grid',
          'row-span-2': layout.layout === 'triple' && position === 0,
        }"
      >
        <button
          type="button"
          class="block size-full focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
          :class="layout.layout === 'single' ? 'min-h-30' : ''"
          :aria-label="layout.hidden && position === tiles.length - 1 ? `Preview ${attachment.name}, ${layout.hidden} more` : `Preview ${attachment.name}`"
          :title="attachment.name"
          @click="open(attachment)"
        >
          <AttachmentThumb :attachment="attachment" :natural="layout.layout === 'single'" />
          <span
            v-if="layout.hidden && position === tiles.length - 1"
            class="absolute inset-0 grid place-items-center bg-black/45 text-lg font-bold text-white"
            aria-hidden="true"
          >+{{ layout.hidden }}</span>
        </button>
        <AttachmentActions :attachment="attachment" :trigger-class="actions" />
      </li>
    </ul>
    <ul v-if="groups.documents.length" class="m-0! flex max-w-full list-none flex-wrap justify-end gap-2 p-0!">
      <AttachmentCard
        v-for="attachment in groups.documents"
        :key="attachment.id"
        :attachment="attachment"
        document
        @open="open(attachment)"
      />
    </ul>
    <ul v-if="groups.files.length" class="m-0! flex max-w-full list-none flex-col items-end gap-1.5 p-0!">
      <AttachmentCard
        v-for="attachment in groups.files"
        :key="attachment.id"
        :attachment="attachment"
        @open="open(attachment)"
      />
    </ul>
    <AttachmentViewer
      v-if="viewing !== undefined"
      :attachments="ordered"
      :initial="viewing"
      @close="viewing = undefined"
    />
  </div>
</template>
