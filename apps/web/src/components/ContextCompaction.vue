<script setup lang="ts">
import type { CompactionEntry } from '../context-usage'
import { computed } from 'vue'
import { compactionText } from '../context-usage'
import { Layers } from '../icons'
import Icon from './Icon.vue'

// Where the coding agent compacted its context. Messages above it stay as they were.
const props = defineProps<{ entry: CompactionEntry }>()
const text = computed(() => compactionText(props.entry))
</script>

<template>
  <div
    class="context-compaction my-4 flex items-center gap-3 text-2xs"
    :class="entry.state === 'failed' ? 'text-coral' : 'text-muted'"
    role="note"
    data-testid="context-compaction"
    :aria-busy="entry.state === 'running'"
  >
    <span class="h-px flex-1 bg-line" aria-hidden="true" />
    <span class="flex min-w-0 max-w-[85%] flex-wrap items-center justify-center gap-x-2 gap-y-0.5 text-center">
      <span class="inline-flex items-center gap-1.5 font-semibold"><Icon :name="Layers" :size="13" />{{ text.title }}</span>
      <span v-if="text.detail">{{ text.detail }}</span>
    </span>
    <span class="h-px flex-1 bg-line" aria-hidden="true" />
  </div>
</template>
