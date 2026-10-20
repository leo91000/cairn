<script setup lang="ts">
import type { ChipTone, SectionId } from '../agent-access'
import { ChevronRight } from '../icons'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'

// Summary cards of what the agent can access and do; each opens its section.
defineProps<{
  groups: {
    title: string
    cards: {
      id: string
      section: SectionId
      title: string
      summary: string
      detail: string
      chip?: { tone: ChipTone, label: string }
      power?: boolean
      action?: string
    }[]
  }[]
}>()
const emit = defineEmits<{ open: [section: SectionId] }>()
</script>

<template>
  <div class="grid min-w-0 gap-7">
    <section
      v-for="group in groups"
      :key="group.title"
      class="grid min-w-0 gap-3.5"
      :aria-label="group.title"
    >
      <h3 class="text-base! font-semibold">
        {{ group.title }}
      </h3>
      <div class="grid grid-cols-3 gap-3.5 compact:grid-cols-2">
        <button
          v-for="card in group.cards"
          :key="card.id"
          type="button"
          class="group grid min-w-0 content-start gap-1.5 rounded-xl border p-4.5 text-left transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
          :class="card.power ? 'border-power/30 bg-power-surface/40 hover:bg-power-surface/70' : 'border-line bg-surface hover:bg-hover/40'"
          @click="emit('open', card.section)"
        >
          <span class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">{{ card.title }}</span>
          <span class="text-sm font-semibold text-ink">{{ card.summary }}</span>
          <span v-if="card.detail" class="line-clamp-2 text-xs leading-relaxed text-muted">{{ card.detail }}</span>
          <span v-if="card.chip" class="mt-0.5 flex"><AccessChip :tone="card.chip.tone">{{ card.chip.label }}</AccessChip></span>
          <span class="mt-1 inline-flex items-center gap-0.5 text-xs font-medium text-accent group-hover:underline">{{ card.action ?? 'Edit' }}<Icon :name="ChevronRight" :size="13" /></span>
        </button>
      </div>
    </section>
  </div>
</template>
