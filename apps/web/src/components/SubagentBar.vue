<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { computed, onBeforeUnmount, ref } from 'vue'
import {
  subagentDots as dots,
  subagentElapsed,
  subagentLabels,
  subagentState,
} from '../subagents'
import SubagentAvatar from './SubagentAvatar.vue'

// The subagents of the current request, beside the composer so they stay in sight while the
// conversation scrolls. Each one opens its full activity; on phones they fold into one button.
const props = defineProps<{ agents: ActivityArtifact[], active: boolean, selected?: string | null }>()
const emit = defineEmits<{ open: [id: string, opener: HTMLElement] }>()
const now = ref(Date.now())
const timer = setInterval(() => now.value = Date.now(), 1000)
onBeforeUnmount(() => clearInterval(timer))

const state = (agent: ActivityArtifact) => subagentState(agent, props.active)
const running = computed(() => props.agents.filter(agent => state(agent) === 'running'))
const first = computed(() => running.value[0] ?? props.agents[0])
</script>

<template>
  <section class="subagent-bar mb-2 flex min-w-0 items-center gap-2 rounded-2xl border border-line bg-surface px-2.5 py-1.5" aria-label="Subagents at work" data-testid="subagent-bar">
    <span class="shrink-0 pl-1 font-heading text-xs font-semibold text-ink phone:hidden">Subagents</span>
    <div class="flex min-w-0 flex-1 gap-1.5 overflow-x-auto [scrollbar-width:none] phone:hidden">
      <button
        v-for="agent in agents"
        :key="agent.id"
        class="inline-flex min-h-8 shrink-0 items-center gap-1.5 rounded-[10px] border py-1 pr-2 pl-1 text-xs hover:bg-hover"
        :class="selected === agent.id ? 'border-accent bg-soft' : 'border-line bg-inset'"
        :aria-label="`${agent.title}, ${subagentLabels[state(agent)]}. Open its activity`"
        :aria-pressed="selected === agent.id"
        data-testid="subagent-chip"
        @click="emit('open', agent.id, $event.currentTarget as HTMLElement)"
      >
        <SubagentAvatar :agent="agent" :size="20" />
        <span class="max-w-40 truncate">{{ agent.title }}</span>
        <span class="size-1.5 shrink-0 rounded-full" :class="dots[state(agent)]" aria-hidden="true" />
        <span v-if="state(agent) === 'running'" class="tabular-nums text-muted" aria-hidden="true">{{ subagentElapsed(agent, active, now) }}</span>
      </button>
    </div>
    <span class="shrink-0 pr-1 text-2xs text-muted phone:hidden">{{ running.length === 1 ? '1 running' : `${running.length} running` }}</span>
    <button
      v-if="first"
      class="hidden min-h-11 min-w-0 flex-1 items-center gap-2 text-left phone:flex"
      :aria-label="`${running.length === 1 ? '1 subagent' : `${running.length} subagents`} working. Open their activity`"
      @click="emit('open', first.id, $event.currentTarget as HTMLElement)"
    >
      <span class="flex shrink-0 -space-x-1.5">
        <SubagentAvatar
          v-for="agent in agents.slice(0, 3)"
          :key="agent.id"
          :agent="agent"
          :size="22"
          class="ring-2 ring-surface"
        />
      </span>
      <span class="min-w-0 flex-1 truncate text-xs"><b class="font-semibold">{{ running.length === 1 ? '1 subagent' : `${running.length} subagents` }}</b> <span class="text-muted">working</span></span>
      <span class="subagent-pulse size-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true" />
    </button>
  </section>
</template>
