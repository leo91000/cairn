<script setup lang="ts">
import type { ActivityArtifact, ActivitySubagent } from '../activity'
import {
  computed,
  onBeforeUnmount,
  ref,
  useId,
  watchEffect,
} from 'vue'
import { teamEntries } from '../activity'
import { ChevronDown, GitFork } from '../icons'
import { liveElapsed } from '../signal'
import ActivityContent from './ActivityContent.vue'
import AgentActions from './AgentActions.vue'
import Icon from './Icon.vue'

// The subagents the agent started together: who works, on what, and for how long. A row expands
// into the subagent's own steps and its report. The card folds once every subagent has finished.
const props = defineProps<{ agents: ActivityArtifact[], active: boolean }>()
const listId = useId()
const toggled = ref<boolean | null>(null)
const expanded = ref(new Set<string>())
const now = ref(Date.now())

type State = ActivitySubagent['state']

const labels: Record<State, string> = {
  running: 'Running',
  done: 'Done',
  failed: 'Failed',
  stopped: 'Stopped',
}
const pills: Record<State, string> = {
  running: 'bg-soft text-accent',
  done: 'bg-variant text-success',
  failed: 'bg-coral-soft text-coral',
  stopped: 'bg-variant text-muted',
}
const colors = ['#2f6f9f', '#7a5aa6', '#12806f', '#a8692a', '#b5475f', '#4b6bb5']

// A subagent still marked as working once the run has ended will not report anymore.
function state(agent: ActivityArtifact): State {
  const value = agent.subagent?.state ?? 'running'
  return value === 'running' && !props.active ? 'stopped' : value
}

const states = computed(() => props.agents.map(state))
const running = computed(() => states.value.filter(value => value === 'running').length)
const failed = computed(() => states.value.filter(value => value === 'failed').length)
const open = computed(() => toggled.value ?? (running.value > 0 || failed.value > 0))
const counts = computed(() => (['running', 'done', 'failed', 'stopped'] as const)
  .map(value => ({ state: value, count: states.value.filter(item => item === value).length }))
  .filter(item => item.count))

let timer: ReturnType<typeof setInterval> | undefined
watchEffect(() => {
  clearInterval(timer)
  timer = running.value ? setInterval(() => now.value = Date.now(), 1000) : undefined
})
onBeforeUnmount(() => clearInterval(timer))

function initials(name: string) {
  const words = name.split(/\s+/).filter(word => /\w/.test(word))
  const letters = words.length > 1 ? words[0]![0]! + words[1]![0]! : name.replace(/\W/g, '').slice(0, 2)
  return letters.toUpperCase() || '·'
}

function firstLine(value: string) {
  return value.split('\n').map(line => line.replace(/^[#>*\s-]+/, '').trim()).find(Boolean) ?? ''
}

// What the subagent does now, or what it reported.
function summary(agent: ActivityArtifact) {
  const subagent = agent.subagent!
  const current = state(agent)
  if (current === 'running') {
    const latest = subagent.steps.findLast(step => step.kind !== 'notice')
    const step = latest && [latest.title, latest.command || latest.subtitle].filter(Boolean).join(' · ')
    return subagent.activity || step || 'Starting…'
  }

  const report = firstLine(subagent.result) || firstLine(subagent.message)
  if (current === 'failed')
    return report || 'Failed without a report'
  if (current === 'stopped')
    return report || 'Stopped before reporting back'
  return report || 'Finished'
}

function elapsed(agent: ActivityArtifact) {
  if (state(agent) === 'running')
    return liveElapsed(agent.time, now.value)
  // liveElapsed formats any span; anchor this one at an arbitrary positive start.
  return agent.durationMs === undefined ? '' : liveElapsed(1, 1 + agent.durationMs)
}

function details(subagent: ActivitySubagent) {
  return [
    subagent.model,
    subagent.toolUses === undefined ? '' : subagent.toolUses === 1 ? '1 tool use' : `${subagent.toolUses.toLocaleString()} tool uses`,
    subagent.tokens === undefined ? '' : `${subagent.tokens.toLocaleString()} tokens`,
  ].filter(Boolean).join(' · ')
}

function toggle(id: string) {
  const next = new Set(expanded.value)
  if (!next.delete(id))
    next.add(id)
  expanded.value = next
}
</script>

<template>
  <section class="subagent-team my-3 overflow-hidden rounded-[18px] border border-line bg-surface" data-testid="subagent-team" aria-label="Subagents">
    <button
      class="flex min-h-12 w-full items-center gap-2.5 px-3 py-2.5 text-left"
      :aria-expanded="open"
      :aria-controls="listId"
      @click="toggled = !open"
    >
      <span class="grid size-6.5 shrink-0 place-items-center rounded-full bg-variant text-muted" aria-hidden="true"><Icon :name="GitFork" :size="13" /></span>
      <span class="min-w-0 flex-1">
        <span class="block font-heading text-base font-semibold text-ink">{{ agents.length === 1 ? '1 subagent' : `${agents.length} subagents` }}</span>
        <span class="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-2xs text-muted">
          <span
            v-for="item in counts"
            :key="item.state"
            class="inline-flex items-center gap-1"
            :class="item.state === 'running' ? 'text-accent' : item.state === 'failed' ? 'text-coral' : ''"
          >
            <span v-if="item.state === 'running'" class="subagent-pulse size-1.5 rounded-full bg-accent" aria-hidden="true" />
            {{ item.count }} {{ labels[item.state].toLowerCase() }}
          </span>
        </span>
      </span>
      <Icon
        :name="ChevronDown"
        :size="18"
        class="shrink-0 text-muted transition-transform"
        :class="open ? 'rotate-180' : ''"
      />
    </button>
    <ul v-if="open" :id="listId" class="m-0! list-none border-t border-line p-0">
      <li
        v-for="(agent, index) in agents"
        :key="agent.id"
        class="border-b border-line last:border-b-0"
        data-testid="subagent"
      >
        <button
          class="flex w-full items-center gap-3 px-3 py-2.5 text-left hover:bg-hover/60"
          :class="expanded.has(agent.id) ? 'bg-inset' : ''"
          :aria-expanded="expanded.has(agent.id)"
          :aria-label="[agent.title, labels[state(agent)], summary(agent)].join(', ')"
          @click="toggle(agent.id)"
        >
          <span class="grid size-7 shrink-0 place-items-center rounded-lg text-[10.5px] font-bold text-white" :style="{ background: colors[index % colors.length] }" aria-hidden="true">{{ initials(agent.title) }}</span>
          <span class="min-w-0 flex-1" aria-hidden="true">
            <span class="flex min-w-0 items-baseline gap-1.5">
              <span class="truncate text-base font-semibold text-ink">{{ agent.title }}</span>
              <span v-if="agent.subagent!.type" class="shrink-0 truncate text-2xs text-muted">{{ agent.subagent!.type }}</span>
              <span v-if="agent.subagent!.background" class="shrink-0 text-2xs text-muted">· background</span>
            </span>
            <span class="block truncate text-2xs text-muted">{{ summary(agent) }}</span>
          </span>
          <span v-if="elapsed(agent)" class="shrink-0 text-2xs tabular-nums text-muted" aria-hidden="true">{{ elapsed(agent) }}</span>
          <span class="inline-flex shrink-0 items-center gap-1.5 rounded-full px-2 py-0.5 text-2xs font-semibold" :class="pills[state(agent)]" aria-hidden="true">
            <span v-if="state(agent) === 'running'" class="subagent-pulse size-1.5 rounded-full bg-current" />{{ labels[state(agent)] }}
          </span>
        </button>
        <div v-if="expanded.has(agent.id)" class="grid gap-2 bg-inset px-3 pb-3 pl-13 phone:pl-3" data-testid="subagent-detail">
          <p v-if="agent.subagent!.prompt" class="m-0! line-clamp-4 whitespace-pre-wrap text-xs text-muted">
            <span class="font-semibold text-ink">Prompt · </span>{{ agent.subagent!.prompt }}
          </p>
          <template v-for="entry in teamEntries([{ kind: 'group', id: agent.id, artifacts: agent.subagent!.steps }])" :key="entry.id">
            <AgentActions v-if="entry.kind === 'group'" :artifacts="entry.artifacts" :active="state(agent) === 'running'" />
            <SubagentTeam v-else-if="entry.kind === 'team'" :agents="entry.agents" :active="active" />
          </template>
          <p v-if="!agent.subagent!.steps.length && state(agent) === 'running'" class="m-0! text-xs text-muted">
            No steps yet.
          </p>
          <div v-if="agent.subagent!.result || agent.subagent!.message" class="rounded-xl border border-line bg-surface px-3 py-2">
            <p class="m-0! text-2xs font-semibold text-muted">
              {{ agent.subagent!.result ? 'Report' : 'Latest message' }}
            </p>
            <ActivityContent :content="agent.subagent!.result || agent.subagent!.message" />
          </div>
          <p v-if="details(agent.subagent!)" class="m-0! text-2xs text-muted">
            {{ details(agent.subagent!) }}
          </p>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.subagent-pulse {
  animation: subagent-pulse 1.4s ease-in-out infinite;
}
@keyframes subagent-pulse {
  50% {
    opacity: 0.35;
    transform: scale(0.7);
  }
}
@media (prefers-reduced-motion: reduce) {
  .subagent-pulse {
    animation: none;
  }
}
</style>
