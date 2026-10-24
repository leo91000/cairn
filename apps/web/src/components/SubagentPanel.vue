<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  useId,
  watch,
} from 'vue'
import { X } from '../icons'
import {
  subagentDots as dots,
  subagentDetails,
  subagentElapsed,
  subagentLabels,
  subagentPills,
  subagentState,
} from '../subagents'
import { iconButton } from '../ui'
import ActivityArtifactCard from './ActivityArtifactCard.vue'
import ActivityContent from './ActivityContent.vue'
import Icon from './Icon.vue'
import SubagentAvatar from './SubagentAvatar.vue'
import SubagentTeam from './SubagentTeam.vue'

// A subagent's full activity beside the conversation: its prompt, every step with its output,
// and its report. Subagents started together share the panel as tabs.
const props = defineProps<{ team: ActivityArtifact[], selected: string, active: boolean }>()
const emit = defineEmits<{ select: [id: string], close: [] }>()
const now = ref(Date.now())
const timer = setInterval(() => now.value = Date.now(), 1000)
onBeforeUnmount(() => clearInterval(timer))
const promptOpen = ref(false)
const opened = ref(new Set<string>())
const panelId = useId()
const content = ref<HTMLElement>()
const tabId = (index: number) => `${panelId}-tab-${index}`

const agent = computed(() => props.team.find(item => item.id === props.selected) ?? props.team[0]!)
const subagent = computed(() => agent.value.subagent!)
const state = computed(() => subagentState(agent.value, props.active))
const meta = computed(() => [
  subagentElapsed(agent.value, props.active, now.value),
  subagent.value.steps.length === 1 ? '1 step' : `${subagent.value.steps.length} steps`,
  subagentDetails(subagent.value),
].filter(Boolean).join(' · '))

watch(() => props.selected, async () => {
  promptOpen.value = false
  opened.value = new Set()
  await nextTick()
  content.value?.scrollTo({ top: 0 })
})

function toggle(id: string) {
  const next = new Set(opened.value)
  if (!next.delete(id))
    next.add(id)
  opened.value = next
}

function tabKey(event: KeyboardEvent, index: number) {
  const last = props.team.length - 1
  const next = event.key === 'ArrowRight'
    ? (index + 1) % props.team.length
    : event.key === 'ArrowLeft'
      ? (index + last) % props.team.length
      : event.key === 'Home' ? 0 : event.key === 'End' ? last : undefined
  if (next === undefined)
    return
  event.preventDefault()
  emit('select', props.team[next]!.id)
  const tabs = (event.currentTarget as HTMLElement).parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')
  tabs?.[next]?.focus()
}
</script>

<template>
  <section class="subagent-panel flex h-full min-h-0 flex-col bg-surface" :aria-label="`${agent.title} activity`" data-testid="subagent-panel">
    <div class="flex shrink-0 items-end gap-1 border-b border-line pl-2 pr-1.5 pt-1.5">
      <div
        v-if="team.length > 1"
        class="flex min-w-0 flex-1 gap-0.5 overflow-x-auto [scrollbar-width:none]"
        role="tablist"
        aria-label="Subagents"
      >
        <button
          v-for="(item, index) in team"
          :id="tabId(index)"
          :key="item.id"
          role="tab"
          class="inline-flex min-h-10 shrink-0 items-center gap-1.5 border-b-2 px-2 text-xs font-semibold"
          :class="item.id === agent.id ? 'border-accent text-ink' : 'border-transparent text-muted hover:text-ink'"
          :aria-selected="item.id === agent.id"
          :aria-controls="panelId"
          :tabindex="item.id === agent.id ? 0 : -1"
          @click="emit('select', item.id)"
          @keydown="tabKey($event, index)"
        >
          <SubagentAvatar :agent="item" :size="16" />
          <span class="max-w-32 truncate">{{ item.title }}</span>
          <span class="size-1.5 shrink-0 rounded-full" :class="dots[subagentState(item, active)]" aria-hidden="true" />
        </button>
      </div>
      <p v-else class="m-0! min-w-0 flex-1 truncate px-1 pb-2.5 text-xs font-semibold text-muted">
        Subagent activity
      </p>
      <button
        :class="iconButton"
        class="mb-1 shrink-0"
        aria-label="Close subagent activity"
        @click="emit('close')"
      >
        <Icon :name="X" :size="16" />
      </button>
    </div>
    <div
      :id="panelId"
      ref="content"
      class="min-h-0 flex-1 overflow-auto overscroll-contain px-4 pt-4 pb-6 [scrollbar-width:thin]"
      :role="team.length > 1 ? 'tabpanel' : undefined"
      :aria-labelledby="team.length > 1 ? tabId(team.indexOf(agent)) : undefined"
      :tabindex="team.length > 1 ? 0 : undefined"
    >
      <header class="flex items-start gap-3">
        <SubagentAvatar :agent="agent" :size="36" />
        <div class="min-w-0 flex-1">
          <h2 class="m-0! truncate text-base">
            {{ agent.title }}
          </h2>
          <p class="m-0! truncate text-2xs text-muted">
            {{ [subagent.type, subagent.background ? 'background' : '', subagent.resumed ? 'resumed by the agent' : 'started by the agent'].filter(Boolean).join(' · ') }}
          </p>
        </div>
        <span class="inline-flex shrink-0 items-center gap-1.5 rounded-full px-2 py-0.5 text-2xs font-semibold" :class="subagentPills[state]">
          <span v-if="state === 'running'" class="subagent-pulse size-1.5 rounded-full bg-current" aria-hidden="true" />{{ subagentLabels[state] }}
        </span>
      </header>
      <p class="mt-2! mb-0! text-2xs tabular-nums text-muted">
        {{ meta }}
      </p>
      <div v-if="subagent.prompt" class="mt-3 rounded-xl border border-line bg-inset px-3 py-2 text-xs text-muted">
        <p class="m-0! whitespace-pre-wrap" :class="promptOpen ? '' : 'line-clamp-3'">
          <span class="font-semibold text-ink">Prompt · </span>{{ subagent.prompt }}
        </p>
        <button v-if="subagent.prompt.length > 160 || subagent.prompt.split('\n').length > 3" class="mt-1 text-2xs font-semibold text-accent" @click="promptOpen = !promptOpen">
          {{ promptOpen ? 'Show less' : 'Show all' }}
        </button>
      </div>
      <ol
        v-if="subagent.steps.length"
        class="m-0! mt-2! list-none p-0"
        aria-label="Steps"
        data-testid="subagent-steps"
      >
        <li v-for="step in subagent.steps" :key="step.id" class="border-b border-line/70 last:border-b-0">
          <SubagentTeam v-if="step.subagent" :agents="[step]" :active="active" />
          <ActivityArtifactCard
            v-else
            :artifact="step"
            :active="state === 'running'"
            :expanded="opened.has(step.id)"
            @toggle="toggle(step.id)"
          />
        </li>
      </ol>
      <p v-else class="mt-4! mb-0! text-xs text-muted">
        {{ state === 'running' ? 'No steps yet.' : 'This subagent reported no steps.' }}
      </p>
      <div v-if="subagent.result || subagent.message" class="mt-4 rounded-xl border border-line px-3 py-2">
        <p class="m-0! text-2xs font-semibold text-muted">
          {{ subagent.result ? 'Report' : 'Latest message' }}
        </p>
        <ActivityContent :content="subagent.result || subagent.message" />
      </div>
    </div>
  </section>
</template>
