<script setup lang="ts">
import type { SkillSourceChange } from '../../../../packages/contracts/contracts'
import { computed, ref } from 'vue'
import { ChevronRight, FileBox } from '../icons'
import Icon from './Icon.vue'

// One file of a source update: its status, then a unified diff. The server already trims
// unchanged stretches into `gap` rows.
const props = defineProps<{ change: SkillSourceChange, open?: boolean }>()
const expanded = ref(props.open ?? false)
const counts = computed(() => ({
  added: props.change.lines?.filter(line => line.kind === 'added').length ?? 0,
  removed: props.change.lines?.filter(line => line.kind === 'removed').length ?? 0,
}))
const status = {
  added: { label: 'Added', tone: 'bg-success/12 text-success' },
  removed: { label: 'Removed', tone: 'bg-danger-surface text-danger' },
  modified: { label: 'Modified', tone: 'bg-warning-surface text-warning' },
}
const prefix = {
  added: '+',
  removed: '−',
  context: ' ',
  gap: '',
}
</script>

<template>
  <section class="overflow-hidden rounded-xl border border-line bg-surface">
    <button
      type="button"
      class="flex min-h-11 w-full items-center gap-2.5 px-3.5 py-2 text-left hover:bg-hover/50"
      :aria-expanded="expanded"
      @click="expanded = !expanded"
    >
      <Icon
        :name="ChevronRight"
        :size="14"
        class="shrink-0 text-subtle transition-transform"
        :class="expanded ? 'rotate-90' : ''"
      />
      <code class="min-w-0 flex-1 truncate text-xs text-ink">{{ change.path }}</code>
      <span v-if="change.lines" class="shrink-0 font-mono text-2xs tabular-nums">
        <span class="text-success">+{{ counts.added }}</span>
        <span class="ml-1.5 text-danger">−{{ counts.removed }}</span>
      </span>
      <span class="shrink-0 rounded-[5px] px-1.5 py-0.5 text-3xs font-semibold" :class="status[change.status].tone">{{ status[change.status].label }}</span>
    </button>
    <div v-if="expanded" class="border-t border-line">
      <p v-if="change.binary" class="m-0! flex items-center gap-2 px-4 py-3.5 text-xs text-muted">
        <Icon :name="FileBox" :size="15" />Binary file{{ change.status === 'modified' ? ' changed' : '' }}
      </p>
      <p v-else-if="!change.lines" class="m-0! px-4 py-3.5 text-xs text-muted">
        This file is too large to show a line diff.
      </p>
      <div v-else class="skill-diff max-h-[420px] overflow-auto overscroll-contain bg-inset py-1 font-mono text-[11.5px] leading-[1.65]">
        <template v-for="(line, index) in change.lines" :key="index">
          <div v-if="line.kind === 'gap'" class="my-1 flex items-center gap-3 bg-variant/60 px-4 py-1 text-2xs text-subtle select-none">
            <span class="h-px flex-1 bg-line" />{{ line.text }}<span class="h-px flex-1 bg-line" />
          </div>
          <div
            v-else
            class="flex min-w-max whitespace-pre"
            :class="line.kind === 'added' ? 'diff-added' : line.kind === 'removed' ? 'diff-removed' : 'text-muted'"
          >
            <span class="w-7 shrink-0 text-center select-none opacity-70" aria-hidden="true">{{ prefix[line.kind] }}</span>
            <span class="pr-4"><span v-if="line.kind !== 'context'" class="sr-only">{{ line.kind === 'added' ? 'Added: ' : 'Removed: ' }}</span>{{ line.text || ' ' }}</span>
          </div>
        </template>
      </div>
    </div>
  </section>
</template>

<style scoped>
.diff-added {
  background: color-mix(in srgb, var(--color-success) 12%, transparent);
  color: light-dark(#0b5a4e, #8fe3cd);
}
.diff-removed {
  background: color-mix(in srgb, var(--color-danger) 11%, transparent);
  color: light-dark(#8c2f1a, #ffb3a3);
}
</style>
