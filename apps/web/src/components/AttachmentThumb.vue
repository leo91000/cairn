<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import { computed, ref, watch } from 'vue'
import { attachmentUrl, useAttachmentMedia } from '../attachment-media'
import { badgeColor, badgeLabel, family } from '../attachments'
import { File } from '../icons'
import Icon from './Icon.vue'

// A square cover thumbnail: the local object URL for drafts, otherwise the cached `?preview`
// (images and PDF first pages), then the original image; other files show their coloured label.
// The caller sizes it; it fills its box.
const props = defineProps<{
  attachment: ChatAttachment
  local?: string
  /** Request the preview immediately instead of waiting for visibility. */
  eager?: boolean
  /** Decorative thumbnails (inside a labelled control) have no alt text. */
  decorative?: boolean
  /** Keep the image's proportions within 240 × 320 (a single image in a sent message). */
  natural?: boolean
}>()
const root = ref<HTMLElement>()
const kind = computed(() => family(props.attachment))
const wantsPreview = computed(() => !props.local && (kind.value === 'image' || kind.value === 'pdf'))
const media = useAttachmentMedia(() => wantsPreview.value ? props.attachment : undefined, { preview: true, target: props.eager ? undefined : root })
const broken = ref(false)
watch(() => [props.attachment.id, props.local], () => broken.value = false)
const source = computed(() => {
  if (broken.value)
    return undefined
  if (props.local)
    return props.local
  if (media.preview.value)
    return media.preview.value
  // SVG and other images the server cannot render still display from the original.
  return kind.value === 'image' && media.previewFailed.value && props.attachment.chatId ? attachmentUrl(props.attachment) : undefined
})
const pending = computed(() => !source.value && !broken.value && wantsPreview.value && !media.previewFailed.value && !!props.attachment.chatId)
// A single image's box, from the size recorded when it was sent: the placeholder and the
// picture take the same space, so the conversation does not move when it appears.
// In rem, as the 240 × 320 bounds of the classes it replaces.
const box = computed(() => {
  const { width, height } = props.attachment
  if (!props.natural || !width || !height)
    return undefined
  const scale = Math.min(1, 240 / width, 320 / height)
  const rem = (pixels: number, min: number, max: number) => `${Math.min(max, Math.max(min, pixels * scale)) / 16}rem`
  return { width: rem(width, 80, 240), height: rem(height, 120, 320) }
})
</script>

<template>
  <span ref="root" class="relative block overflow-hidden bg-variant" :class="natural ? '' : 'size-full'">
    <img
      v-if="source"
      :src="source"
      :alt="decorative ? '' : attachment.name"
      class="block object-cover"
      :class="!natural ? 'size-full' : box ? 'max-w-full' : 'h-auto max-h-80 min-h-30 w-auto min-w-20 max-w-60'"
      :style="box"
      draggable="false"
      @error="broken = true"
    >
    <span
      v-else-if="pending"
      class="block animate-pulse bg-variant"
      :class="!natural ? 'size-full' : box ? 'max-w-full' : 'h-40 w-60 max-w-full'"
      :style="box"
    />
    <span
      v-else
      class="grid place-items-center text-[9px] font-extrabold text-white"
      :class="natural ? 'h-30 w-60 max-w-full' : 'size-full'"
      :style="{ backgroundColor: badgeColor(attachment) }"
      :role="decorative ? undefined : 'img'"
      :aria-label="decorative ? undefined : attachment.name"
    >
      <template v-if="badgeLabel(attachment.name)">{{ badgeLabel(attachment.name) }}</template>
      <Icon v-else :name="File" :size="14" />
    </span>
  </span>
</template>
