<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { computed } from 'vue'
import UiNotice from './UiNotice.vue'

const props = defineProps<{ notice: ActivityArtifact }>()
const detail = computed(() => props.notice.blocks.find(block => block.language === 'plaintext')?.code || props.notice.subtitle)
</script>

<template>
  <UiNotice
    role="note"
    class="chat-notice my-4 text-xs"
    :tone="notice.status === 'error' ? 'danger' : 'warning'"
    :title="notice.title"
  >
    <p v-if="detail && detail !== notice.title" class="whitespace-pre-wrap">
      {{ detail }}
    </p>
  </UiNotice>
</template>
