<script setup lang="ts">
import type { ClassValue } from 'vue'
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import { computed, inject } from 'vue'
import { notify } from '../api'
import {
  attachmentUrl,
  canCopyImages,
  canShareFiles,
  copyImage,
  reuseAttachmentKey,
  shareAttachment,
} from '../attachment-media'
import { isImage, subtitle } from '../attachments'
import {
  Copy,
  Download,
  MoreHorizontal,
  Reply,
  Share2,
} from '../icons'
import Icon from './Icon.vue'
import MenuButton from './MenuButton.vue'

// C': quick actions on a sent attachment, without opening it.
const props = defineProps<{ attachment: ChatAttachment, triggerClass?: ClassValue }>()
const reuse = inject(reuseAttachmentKey, null)
const share = canShareFiles()
const copy = computed(() => canCopyImages() && isImage(props.attachment))
const reusable = computed(() => !!reuse && !!props.attachment.chatId && reuse.chatId() === props.attachment.chatId)

async function shareFile() {
  try {
    await shareAttachment(props.attachment)
  }
  catch (error) { notify((error as Error).message || 'Unable to share this file.') }
}

async function copyFile() {
  try {
    await copyImage(props.attachment)
    notify('Image copied')
  }
  catch { notify('Unable to copy this image.') }
}
</script>

<template>
  <MenuButton
    :label="`More actions for ${attachment.name}`"
    :menu-label="`Actions for ${attachment.name}`"
    title="More actions"
    :trigger-class="triggerClass"
    :width="248"
  >
    <template #trigger>
      <Icon :name="MoreHorizontal" :size="16" />
    </template>
    <template #default="{ select, close }">
      <div class="mb-1 border-b border-line px-2.5 pt-1 pb-2" aria-hidden="true">
        <p class="m-0! text-xs font-semibold wrap-anywhere">
          {{ attachment.name }}
        </p>
        <p class="m-0! text-2xs text-muted">
          {{ subtitle(attachment) }}
        </p>
      </div>
      <a
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        :href="attachmentUrl(attachment)"
        :download="attachment.name"
        @click="close()"
      ><Icon :name="Download" :size="16" />Download</a>
      <button
        v-if="share"
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(shareFile)"
      >
        <Icon :name="Share2" :size="16" />Share
      </button>
      <button
        v-if="copy"
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(copyFile)"
      >
        <Icon :name="Copy" :size="16" />Copy image
      </button>
      <button
        v-if="reusable"
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(() => reuse!.reuse(attachment), false)"
      >
        <Icon :name="Reply" :size="16" />Reuse in new message
      </button>
    </template>
  </MenuButton>
</template>
