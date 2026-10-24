<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import {
  computed,
  inject,
  onBeforeUnmount,
  ref,
  useId,
  watchEffect,
} from 'vue'
import { RouterLink } from 'vue-router'
import { teamEntries } from '../activity'
import {
  ChevronDown,
  GitFork,
  Maximize2,
} from '../icons'
import {
  delegatedChatsKey,
  openSubagentKey,
  subagentDetails,
  subagentElapsed,
  subagentLabels,
  subagentPills,
  subagentState,
  subagentSummary,
} from '../subagents'
import { chatPath } from '../workspace-links'
import ActivityContent from './ActivityContent.vue'
import AgentActions from './AgentActions.vue'
import Icon from './Icon.vue'
import SubagentMark from './SubagentMark.vue'

// The subagents the agent started together: who works, on what, and for how long. A row expands
// into the subagent's own steps and its report. The card folds once every subagent has finished.
const props = defineProps<{ agents: ActivityArtifact[], active: boolean }>()
const openSubagent = inject(openSubagentKey, null)
const delegated = inject(delegatedChatsKey, null)
const listId = useId()
const toggled = ref<boolean | null>(null)
const expanded = ref(new Set<string>())
const now = ref(Date.now())
const labels = subagentLabels
const pills = subagentPills

const state = (agent: ActivityArtifact) => subagentState(agent, props.active, delegated?.value)
const summary = (agent: ActivityArtifact) => subagentSummary(agent, props.active, delegated?.value)
const elapsed = (agent: ActivityArtifact) => subagentElapsed(agent, props.active, now.value)
const states = computed(() => props.agents.map(state))
const running = computed(() => states.value.filter(value => value === 'running').length)
const failed = computed(() => states.value.filter(value => value === 'failed').length)
const open = computed(() => toggled.value ?? (running.value > 0 || failed.value > 0))
// Delegated tasks are conversations of their own; native subagents work inside this one.
const heading = computed(() => {
  const delegated = props.agents.filter(agent => agent.subagent?.delegation).length
  const noun = delegated === props.agents.length ? 'delegated task' : 'subagent'
  return props.agents.length === 1 ? `1 ${noun}` : `${props.agents.length} ${noun}s`
})
const counts = computed(() => (['running', 'done', 'failed', 'stopped'] as const)
  .map(value => ({ state: value, count: states.value.filter(item => item === value).length }))
  .filter(item => item.count))

let timer: ReturnType<typeof setInterval> | undefined
watchEffect(() => {
  clearInterval(timer)
  timer = running.value ? setInterval(() => now.value = Date.now(), 1000) : undefined
})
onBeforeUnmount(() => clearInterval(timer))

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
        <span class="block font-heading text-base font-semibold text-ink">{{ heading }}</span>
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
        v-for="agent in agents"
        :key="agent.id"
        class="border-b border-line last:border-b-0"
        data-testid="subagent"
      >
        <component
          :is="agent.subagent!.delegation ? RouterLink : 'button'"
          :to="agent.subagent!.delegation ? chatPath({ id: agent.subagent!.delegation.chatId }) : undefined"
          class="flex w-full items-center gap-3 px-3 py-2.5 text-left hover:bg-hover/60"
          :class="expanded.has(agent.id) ? 'bg-inset' : ''"
          :aria-expanded="agent.subagent!.delegation ? undefined : expanded.has(agent.id)"
          :aria-label="[agent.title, labels[state(agent)], summary(agent)].join(', ')"
          @click="!agent.subagent!.delegation && toggle(agent.id)"
        >
          <SubagentMark :agent="agent" :size="28" />
          <span class="min-w-0 flex-1" aria-hidden="true">
            <span class="flex min-w-0 items-baseline gap-1.5">
              <span class="max-w-full shrink-0 truncate text-base font-semibold text-ink">{{ agent.title }}</span>
              <span v-if="agent.subagent!.type" class="min-w-0 truncate text-2xs text-muted phone:hidden">{{ agent.subagent!.type }}</span>
              <span v-if="agent.subagent!.background" class="shrink-0 text-2xs text-muted phone:hidden">· background</span>
            </span>
            <span class="block truncate text-2xs text-muted">{{ summary(agent) }}</span>
          </span>
          <span v-if="elapsed(agent)" class="shrink-0 text-2xs tabular-nums text-muted" aria-hidden="true">{{ elapsed(agent) }}</span>
          <span class="inline-flex shrink-0 items-center gap-1.5 rounded-full px-2 py-0.5 text-2xs font-semibold" :class="pills[state(agent)]" aria-hidden="true">
            <span v-if="state(agent) === 'running'" class="subagent-pulse size-1.5 rounded-full bg-current" />{{ labels[state(agent)] }}
          </span>
        </component>
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
          <div v-if="openSubagent || subagentDetails(agent.subagent!)" class="flex flex-wrap items-center justify-between gap-2">
            <p class="m-0! text-2xs text-muted">
              {{ subagentDetails(agent.subagent!) }}
            </p>
            <button
              v-if="openSubagent"
              class="inline-flex min-h-8 items-center gap-1.5 rounded-lg border border-line bg-surface px-2.5 text-2xs font-semibold text-ink hover:bg-hover phone:min-h-11"
              data-testid="open-subagent"
              @click="openSubagent(agent.id, $event.currentTarget as HTMLElement)"
            >
              <Icon :name="Maximize2" :size="12" />Open full activity
            </button>
          </div>
        </div>
      </li>
    </ul>
  </section>
</template>
