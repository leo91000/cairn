<script setup lang="ts">
import type { Deliverable } from '../../shared/artifacts'
import { computed } from 'vue'
import { artifactUrl, fileSize } from '../../shared/artifacts'
import { FileCode, FileText, Play } from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{ items: Deliverable[] }>()
defineEmits<{ open: [item: Deliverable] }>()
const groups = computed(() => {
  const groups = new Map<string, Deliverable[]>()
  for (const item of props.items) {
    const files = groups.get(item.group) ?? []
    files.push(item)
    groups.set(item.group, files)
  }

  return [...groups].map(([title, items]) => ({ title, items }))
})
</script>

<template>
  <div class="grid gap-5">
    <section v-for="group in groups" :key="group.title">
      <p v-if="groups.length > 1 && group.title" class="mb-2 text-xs text-muted">
        {{ group.title }}
      </p>
      <ul class="m-0! grid list-none grid-cols-[repeat(auto-fill,minmax(min(100%,150px),1fr))] gap-3 p-0!" aria-label="Deliverables">
        <li v-for="item in group.items" :key="item.id" class="min-w-0">
          <button class="group flex h-full w-full flex-col overflow-hidden rounded-xl bg-surface text-left ring-1 ring-line/50 transition hover:bg-hover hover:ring-accent/50 focus-visible:outline-2 focus-visible:outline-accent" :aria-label="`Open ${item.title}`" @click="$emit('open', item)">
            <div class="relative flex aspect-[16/10] w-full items-center justify-center overflow-hidden bg-soft">
              <img
                v-if="item.previewStatus === 'ready' || item.kind === 'image'"
                :src="artifactUrl(item, item.previewStatus === 'ready' ? 'preview' : undefined)"
                :alt="item.title"
                loading="lazy"
                class="size-full object-contain transition-transform duration-300 group-hover:scale-[1.03]"
              >
              <p v-else-if="item.excerpt" class="m-0! h-full w-full overflow-hidden whitespace-pre-wrap break-words p-4 text-[10px] leading-relaxed text-subtle" :class="item.kind === 'code' ? 'font-mono' : ''">
                {{ item.kind === 'markdown' ? item.excerpt.replace(/^#{1,6}\s+/gm, '').replaceAll('**', '') : item.excerpt }}
              </p>
              <Icon
                v-else
                :name="item.kind === 'code' ? FileCode : item.kind === 'video' || item.kind === 'audio' ? Play : FileText"
                :size="32"
                class="text-accent/70"
              />
              <span v-if="item.kind === 'video'" class="absolute inset-0 grid place-items-center"><span class="grid size-11 place-items-center rounded-full bg-black/50 text-white backdrop-blur-sm"><Icon :name="Play" :size="22" /></span></span>
              <span v-if="item.duration" class="absolute right-2 bottom-2 rounded-md bg-black/65 px-1.5 py-0.5 text-[10px] text-white">{{ Math.floor(item.duration / 60) }}:{{ Math.floor(item.duration % 60).toString().padStart(2, '0') }}</span>
              <span v-if="item.version > 1" class="absolute top-2 right-2 rounded-md bg-surface/95 px-1.5 py-0.5 text-[10px] font-medium text-muted">v{{ item.version }}</span>
            </div>
            <div class="w-full min-w-0 px-3 py-2.5">
              <p class="m-0! truncate text-xs font-semibold text-ink" :title="item.title">
                {{ item.title }}
                <span v-if="item.visibility === 'public'" class="ml-1 text-[10px] font-normal text-accent">Public</span>
              </p>
              <p class="m-0! mt-1! truncate text-[10px] text-muted">
                {{ item.name.split('.').at(-1)?.toUpperCase() }} · {{ fileSize(item.size) }}<span v-if="item.width && item.height"> · {{ item.width }} × {{ item.height }}</span>
              </p>
            </div>
          </button>
        </li>
      </ul>
    </section>
  </div>
</template>
