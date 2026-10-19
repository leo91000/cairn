<script setup lang="ts">
import { computed } from 'vue'
import { splitName } from '../attachments'

// Middle ellipsis: the base truncates, the extension always stays visible. The full name is the tooltip.
const props = defineProps<{ name: string }>()
const parts = computed(() => splitName(props.name))
</script>

<template>
  <span class="flex min-w-0 whitespace-nowrap" :title="name">
    <span class="min-w-0 truncate">{{ parts.base }}</span><span v-if="parts.ext" class="shrink-0">{{ parts.ext }}</span>
  </span>
</template>
