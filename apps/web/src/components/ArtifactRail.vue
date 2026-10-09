<script setup lang="ts">
import type { Deliverable } from '../../../../packages/contracts/artifacts'
import { fileSize } from '../../../../packages/contracts/artifacts'
import { artifactUrl } from '../deliverables'
import {
  ChevronRight,
  FileCode,
  FileText,
  Play,
} from '../icons'
import Icon from './Icon.vue'

// The transcript is a reader, not a file gallery: one compact rail of rows, as on Android.
// Full previews, groups and versions remain in Files.
defineProps<{ items: Deliverable[] }>()
defineEmits<{ open: [item: Deliverable] }>()
</script>

<template>
  <ul class="artifact-rail m-0! flex list-none snap-x gap-3 overflow-x-auto p-0! [scrollbar-width:none]" aria-label="Deliverables">
    <li v-for="item in items" :key="item.id" class="w-70 max-w-full shrink-0 snap-start">
      <button class="flex min-h-18 w-full items-center gap-2.5 border-y border-line py-2.5 text-left hover:bg-hover/40 focus-visible:outline-2 focus-visible:outline-accent" :aria-label="`Open ${item.title}`" @click="$emit('open', item)">
        <span class="relative grid size-11 shrink-0 place-items-center overflow-hidden rounded-md bg-surface text-accent">
          <img
            v-if="item.previewStatus === 'ready' || item.kind === 'image'"
            :src="artifactUrl(item, item.previewStatus === 'ready' ? 'preview' : undefined)"
            alt=""
            loading="lazy"
            class="size-full object-cover"
          >
          <Icon v-else :name="item.kind === 'code' ? FileCode : item.kind === 'video' || item.kind === 'audio' ? Play : FileText" :size="22" />
        </span>
        <span class="min-w-0 flex-1">
          <span class="line-clamp-2 text-base font-semibold text-ink">{{ item.title }}</span>
          <span class="mt-0.75 block truncate text-2xs text-muted">
            {{ item.name.split('.').at(-1)?.toUpperCase() || item.kind.toUpperCase() }} · {{ fileSize(item.size) }}<template v-if="item.version > 1"> · v{{ item.version }}</template><span v-if="item.visibility === 'public'" class="text-accent"> · Public</span>
          </span>
        </span>
        <Icon :name="ChevronRight" :size="16" class="shrink-0 text-muted" />
      </button>
    </li>
  </ul>
</template>
