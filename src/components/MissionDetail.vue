<script setup lang="ts">
import type { RunListItem, Task } from '../../shared/contracts'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { api, date, state } from '../api'
import {
  Archive,
  ArrowUpRight,
  Check,
  ChevronDown,
  ChevronRight,
  Clock,
  Copy,
  MoreHorizontal,
  Pause,
  Pencil,
  Play,
  Trash2,
  X,
} from '../icons'
import {
  activeRun,
  describeSchedule,
  elapsedMinutes,
  runDuration,
  successRate,
  upcomingStamp,
} from '../missions'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import ProjectLabel from './ProjectLabel.vue'

// One mission as on Android: who runs it, when, what it asks, how it went, and what to do next.
const props = defineProps<{
  task: Task
  latest?: RunListItem
  sheet?: boolean
  busy?: boolean
}>()
const emit = defineEmits<{
  run: []
  edit: []
  pause: []
  duplicate: []
  archive: []
  remove: []
  close: []
}>()
const history = ref<RunListItem[] | null>(null)
const occurrences = ref<number[]>([])
const promptOpen = ref(false)
const menu = ref(false)
const agent = computed(() => state.agents.find(agent => agent.id === props.task.agentId))
const project = computed(() => state.projects.find(project => project.id === props.task.projectId))
const wording = computed(() => describeSchedule(props.task))
const time = computed(() => wording.value.match(/\d{2}:\d{2}$/)?.[0])
const rate = computed(() => history.value && successRate(history.value))
let timer: ReturnType<typeof setInterval> | undefined

async function loadHistory() {
  try {
    history.value = await api<RunListItem[]>(`/runs?taskId=${encodeURIComponent(props.task.id)}&limit=10`)
  }
  catch { history.value ??= [] }
}

watch(() => props.task.id, () => {
  history.value = null
  promptOpen.value = false
  clearInterval(timer)
  void loadHistory()
  timer = setInterval(() => !document.hidden && loadHistory(), 10000)
}, { immediate: true })
watch(() => [props.task.id, props.task.cron, props.task.timezone, props.task.enabled, props.task.archived] as const, async ([, cron, timezone, enabled, archived]) => {
  occurrences.value = []
  if (!cron || !enabled || archived)
    return
  try {
    occurrences.value = (await api<{ occurrences: number[] }>('/schedule/preview', { method: 'POST', body: JSON.stringify({ cron, timezone }) })).occurrences.slice(0, 2)
  }
  catch { /* The schedule wording remains. */ }
}, { immediate: true })
watch(() => props.latest?.status, (now, before) => {
  if (now !== before)
    void loadHistory()
})
onBeforeUnmount(() => clearInterval(timer))

function act(event: 'duplicate' | 'archive' | 'remove') {
  menu.value = false
  if (event === 'duplicate')
    emit('duplicate')
  else if (event === 'archive')
    emit('archive')
  else
    emit('remove')
}

const tile = (status: RunListItem['status']) => status === 'succeeded' ? { icon: Check, class: 'bg-success/14 text-success' } : status === 'failed' || status === 'interrupted' ? { icon: X, class: 'bg-coral/14 text-coral' } : activeRun(status) ? { icon: Play, class: 'bg-accent/14 text-accent' } : { icon: Pause, class: 'bg-muted/14 text-muted' }
const statusLabels: Record<RunListItem['status'], string> = {
  queued: 'Queued',
  running: 'Running',
  succeeded: 'Finished',
  failed: 'Failed',
  cancelled: 'Cancelled',
  interrupted: 'Interrupted',
}
</script>

<template>
  <article class="mission-detail" data-testid="mission-detail" :aria-label="task.name">
    <div class="flex items-center gap-2.5">
      <AgentAvatar :name="agent?.name ?? '?'" :identity="task.agentId" :size="30" />
      <div class="min-w-0 flex-1">
        <p class="m-0! truncate text-sm font-medium text-muted">
          {{ agent?.name ?? 'Deleted agent' }}
        </p>
        <ProjectLabel :name="project?.name ?? 'All allowed projects'" :identity="project?.id" />
      </div>
      <div class="relative">
        <button
          class="grid size-10 place-items-center rounded-full text-muted hover:bg-hover hover:text-ink"
          aria-label="Mission actions"
          :aria-expanded="menu"
          @click="menu = !menu"
        >
          <Icon :name="MoreHorizontal" :size="19" />
        </button>
        <div v-if="menu" class="absolute top-11 right-0 z-20 w-56 rounded-xl border border-line bg-raised p-1.5 shadow-[0_9px_25px_#0002]" role="menu">
          <button
            class="mission-menu-item"
            role="menuitem"
            :disabled="busy"
            @click="act('duplicate')"
          >
            <Icon :name="Copy" :size="15" />Duplicate (paused)
          </button>
          <button
            class="mission-menu-item"
            role="menuitem"
            :disabled="busy"
            @click="act('archive')"
          >
            <Icon :name="Archive" :size="15" />{{ task.archived ? 'Restore (paused)' : 'Archive' }}
          </button>
          <RouterLink
            v-if="latest"
            class="mission-menu-item"
            role="menuitem"
            :to="`/runs/${latest.id}`"
          >
            <Icon :name="ArrowUpRight" :size="15" />Open the latest run
          </RouterLink>
          <button class="mission-menu-item text-danger" role="menuitem" @click="act('remove')">
            <Icon :name="Trash2" :size="15" />Delete
          </button>
        </div>
      </div>
      <button
        v-if="sheet"
        class="grid size-10 place-items-center rounded-full text-muted hover:bg-hover hover:text-ink"
        aria-label="Close mission"
        @click="emit('close')"
      >
        <Icon :name="X" :size="19" />
      </button>
    </div>
    <h2 class="mt-3! mb-0! font-heading text-[26px] leading-[1.2] font-bold tracking-[-0.6px] wrap-anywhere">
      {{ task.name }}
    </h2>
    <p v-if="task.tags.length" class="mt-1! mb-0! text-xs text-muted">
      {{ task.tags.join(' · ') }}
    </p>
    <section class="mt-4 flex items-center gap-3 rounded-[22px] border border-line bg-surface p-4" aria-label="Schedule">
      <div class="min-w-0 flex-1">
        <p class="mission-eyebrow">
          {{ time ? wording.replace(` · ${time}`, '').replace(/^Paused · /, '') : 'Schedule' }}
        </p>
        <p class="m-0! font-heading" :class="time ? 'text-[32px] leading-[1.15] font-bold tracking-[-0.8px]' : 'text-lg font-semibold'">
          {{ time ?? wording }}
        </p>
        <p v-if="task.cron" class="m-0! text-2xs text-muted">
          {{ task.cron }} · {{ task.timezone }}
        </p>
      </div>
      <div v-if="occurrences.length" class="text-right">
        <p class="m-0! text-xs text-muted">
          Next
        </p>
        <p class="mt-1.5! mb-0! flex justify-end gap-1.5">
          <span
            v-for="(value, index) in occurrences"
            :key="value"
            class="rounded-full px-2.5 py-1 text-xs font-semibold"
            :class="index ? 'bg-variant text-ink' : 'bg-ink text-canvas'"
            :title="date(value)"
          >{{ upcomingStamp(value).split(' · ')[0] }}</span>
        </p>
      </div>
    </section>
    <section class="mt-3 rounded-[22px] bg-variant p-4" aria-label="Mission brief">
      <button class="flex w-full items-center text-left" :aria-expanded="promptOpen" @click="promptOpen = !promptOpen">
        <span class="mission-eyebrow flex-1">Mission</span>
        <Icon :name="promptOpen ? ChevronDown : ChevronRight" :size="16" class="text-muted" /><span class="sr-only">{{ promptOpen ? 'Collapse the mission' : 'Show the whole mission' }}</span>
      </button>
      <Markdown v-if="promptOpen" class="mt-1.5" :content="task.prompt" />
      <p v-else class="mt-1.5! mb-0! line-clamp-3 cursor-pointer text-base whitespace-pre-wrap text-ink" @click="promptOpen = true">
        {{ task.prompt }}
      </p>
    </section>
    <div class="mt-5 mb-1 flex items-center">
      <p class="mission-eyebrow m-0! flex-1">
        History
      </p>
      <p v-if="rate !== null && rate !== undefined" class="m-0! text-sm font-semibold" :class="rate >= 50 ? 'text-success' : 'text-coral'">
        {{ rate }}% success
      </p>
    </div>
    <p v-if="history === null" class="my-2! text-sm text-muted" role="status">
      Loading history…
    </p>
    <p v-else-if="!history.length" class="my-2! text-sm text-muted">
      No runs yet.
    </p>
    <ul v-else class="m-0! list-none p-0!" aria-label="Run history">
      <li v-for="item in history" :key="item.id">
        <RouterLink :to="`/runs/${item.id}`" class="flex min-h-14 items-center gap-3 rounded-xl py-1.5 hover:bg-hover/60">
          <span class="grid size-7.5 shrink-0 place-items-center rounded-[10px]" :class="tile(item.status).class"><Icon :name="tile(item.status).icon" :size="14" /></span>
          <span class="min-w-0 flex-1">
            <span class="block text-base font-semibold text-ink">{{ date(item.startedAt ?? item.createdAt) }}</span>
            <span class="block truncate text-xs text-muted">{{ item.outcome?.reason?.split('\n')[0] || statusLabels[item.status] }}</span>
          </span>
          <span class="shrink-0 text-xs text-muted">{{ activeRun(item.status) ? elapsedMinutes(item.startedAt) : runDuration(item) }}</span>
          <Icon :name="ChevronRight" :size="16" class="shrink-0 text-muted" />
        </RouterLink>
      </li>
    </ul>
    <div class="mt-4 flex items-center gap-2.5">
      <button
        class="mission-round"
        aria-label="Edit mission"
        :disabled="busy"
        @click="emit('edit')"
      >
        <Icon :name="Pencil" :size="18" />
      </button>
      <button
        v-if="!task.archived && task.cron"
        class="mission-round"
        :aria-label="task.enabled ? 'Pause schedule' : 'Resume schedule'"
        :disabled="busy"
        @click="emit('pause')"
      >
        <Icon :name="task.enabled ? Pause : Clock" :size="18" />
      </button>
      <button class="flex h-13 min-w-0 flex-1 items-center justify-center gap-2 rounded-full bg-ink px-5 text-sm font-semibold text-canvas hover:opacity-90 disabled:opacity-40" :disabled="busy || task.archived" @click="emit('run')">
        <Icon :name="Play" :size="15" />{{ busy ? 'Starting…' : 'Run now' }}
      </button>
    </div>
  </article>
</template>

<style scoped>
.mission-eyebrow {
  margin: 0;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 1.2px;
  text-transform: uppercase;
  color: var(--color-muted);
}
.mission-round {
  display: grid;
  width: 52px;
  height: 52px;
  flex-shrink: 0;
  place-items: center;
  border: 1px solid var(--color-line);
  border-radius: 9999px;
}
.mission-round:hover:not(:disabled) {
  background: var(--color-hover);
}
.mission-round:disabled {
  opacity: 0.4;
}
.mission-menu-item {
  display: flex;
  width: 100%;
  align-items: center;
  gap: 9px;
  border-radius: 8px;
  padding: 10px;
  text-align: left;
  font-size: 13px;
}
.mission-menu-item:hover:not(:disabled) {
  background: var(--color-hover);
}
</style>
