<script setup lang="ts">
import type { ChatAttachment } from '../../../../packages/contracts/chats'
import { computed } from 'vue'
import { badgeColor, badgeLabel } from '../attachments'
import { File } from '../icons'
import Icon from './Icon.vue'

// The coloured type tile: white bold extension on the family colour, or a file icon without one.
// `chip` is the small label drawn over document previews.
const props = defineProps<{
  attachment: Pick<ChatAttachment, 'name'> & Partial<Pick<ChatAttachment, 'kind' | 'mediaType'>>
  chip?: boolean
  large?: boolean
}>()
const label = computed(() => badgeLabel(props.attachment.name))
</script>

<template>
  <span
    aria-hidden="true"
    class="inline-grid shrink-0 place-items-center font-extrabold text-white"
    :class="chip
      ? 'rounded-[4px] px-1.25 py-0.5 text-[8.5px] leading-none'
      : large
        ? 'h-28 w-24 rounded-[14px] text-xl tracking-[.5px]'
        : 'h-11 w-9.5 rounded-[7px] text-[9.5px] tracking-[.3px]'"
    :style="{ backgroundColor: badgeColor(attachment) }"
  >
    <template v-if="label">{{ label }}</template>
    <Icon v-else :name="File" :size="chip ? 10 : large ? 36 : 16" />
  </span>
</template>
