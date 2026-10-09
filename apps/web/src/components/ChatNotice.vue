<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { computed } from 'vue'
import { CircleAlert } from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{ notice: ActivityArtifact }>()
const detail = computed(() => props.notice.blocks.find(block => block.language === 'plaintext')?.code || props.notice.subtitle)
</script>

<template>
  <div role="note" class="chat-notice my-4 flex min-w-0 items-start gap-2.5 rounded-lg border px-3 py-2.5 text-xs" :class="notice.status === 'error' ? 'border-danger/30 bg-danger-surface text-danger' : 'border-warning/30 bg-warning-surface text-warning'">
    <Icon :name="CircleAlert" :size="15" class="mt-0.5 shrink-0" />
    <div class="min-w-0 wrap-anywhere">
      <p class="font-medium">
        {{ notice.title }}
      </p>
      <p v-if="detail && detail !== notice.title" class="mt-1 whitespace-pre-wrap">
        {{ detail }}
      </p>
    </div>
  </div>
</template>
