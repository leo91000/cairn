<script setup lang="ts">
import type { RunListItem, Task } from '../../shared/contracts'
import type { MissionFilter } from '../missions'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Empty from '../components/Empty.vue'
import Icon from '../components/Icon.vue'
import MissionDetail from '../components/MissionDetail.vue'
import Modal from '../components/Modal.vue'
import ProjectLabel from '../components/ProjectLabel.vue'
import TaskEditor from '../components/TaskEditor.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  Clock,
  Pause,
  Play,
  Plus,
  Search,
} from '../icons'
import {
  activeRun,
  describeSchedule,
  elapsedMinutes,
  missionFilter,
  missionFilters,
  missionGroup,
  missionOrder,
  upcomingStamp,
} from '../missions'

// Missions as on Android: cards with their schedule and recent runs, and one mission's detail
// beside the list on wide screens or in a sheet on phones. Runs open on their own page.
const router = useRouter()
const route = useRoute()
const latestRuns = ref<RunListItem[]>([])
const recentRuns = ref<RunListItem[]>([])
const loading = ref(true)
const query = ref('')
const searching = ref(false)
const searchInput = ref<HTMLInputElement>()
const filter = ref<MissionFilter>('all')
const editor = ref<Task | true | null>(null)
const error = ref('')
const busy = ref('')
const deleting = ref<Task | null>(null)
const now = ref(Date.now())
const wideQuery = window.matchMedia('(width > 900px)')
const wide = ref(wideQuery.matches)
const onWide = () => wide.value = wideQuery.matches
let disposed = false
let refreshing = false
let timer: ReturnType<typeof setInterval>
const latest = computed(() => new Map(latestRuns.value.map(run => [run.taskId, run])))
const history = computed(() => {
  const runs = new Map<string, RunListItem[]>()
  for (const run of recentRuns.value.filter(run => run.trigger !== 'chat'))
    runs.set(run.taskId, [...runs.get(run.taskId) ?? [], run])
  return runs
})
const visible = computed(() => missionOrder(state.tasks.filter(task => missionFilter(task, filter.value) && `${task.name} ${task.tags.join(' ')}`.toLowerCase().includes(query.value.trim().toLowerCase())), latest.value))
const summary = computed(() => {
  const active = state.tasks.filter(task => !task.archived && task.enabled).length
  const next = Math.min(...state.tasks.filter(task => task.enabled && !task.archived && task.nextRun).map(task => task.nextRun!))
  return [`${active} active`, Number.isFinite(next) ? `next: ${upcomingStamp(next, now.value).replace(/^\w/, letter => letter.toLowerCase())}` : ''].filter(Boolean).join(' · ')
})
const chosen = computed(() => state.tasks.find(task => task.id === route.query.task))
// Wide screens always show a mission; phones open one in a sheet.
const shown = computed(() => chosen.value ?? (wide.value ? visible.value[0] : undefined))
const quiet = (task: Task) => task.archived || (!task.enabled && !!task.cron)

function select(task: Task) {
  void router.replace({ query: { ...route.query, task: task.id } })
}

function closeSheet() {
  const { task: _task, ...rest } = route.query
  void router.replace({ query: rest })
}

async function toggleSearch() {
  searching.value = !searching.value
  if (searching.value) {
    await nextTick()
    searchInput.value?.focus()
  }
}

async function run(task: Task) {
  busy.value = task.id
  error.value = ''
  try {
    const started = await api<RunListItem>(`/tasks/${task.id}/run`, { method: 'POST' })
    notify('Mission added to the queue')
    await router.push(`/runs/${started.id}`)
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = '' }
}

async function save(task: Task, message: string, method = 'PUT', path = `/tasks/${task.id}`) {
  try {
    await api(path, { method, body: JSON.stringify(task) })
    await refresh()
    notify(message)
  }
  catch (e) { error.value = (e as Error).message }
}

const pause = (task: Task) => save({ ...task, enabled: !task.enabled }, task.enabled ? 'Schedule paused' : 'Schedule resumed')
const archive = (task: Task) => save({ ...task, archived: !task.archived, enabled: false }, task.archived ? 'Mission restored with its schedule paused' : 'Mission archived')

function duplicate(task: Task) {
  return save({
    ...task,
    name: `${task.name.slice(0, 92)} (copy)`,
    enabled: false,
    archived: false,
  }, 'Mission duplicated with its schedule paused', 'POST', '/tasks')
}

async function remove() {
  try {
    await api(`/tasks/${deleting.value!.id}`, { method: 'DELETE' })
    if (route.query.task === deleting.value!.id)
      closeSheet()
    deleting.value = null
    await refresh()
    notify('Mission removed')
  }
  catch (e) { error.value = (e as Error).message }
}

function saved(task: Task) {
  query.value = ''
  filter.value = task.archived ? 'archived' : 'all'
  select(task)
}

async function loadActivity() {
  if (refreshing || disposed)
    return
  refreshing = true
  try {
    const [runs, recent, tasks] = await Promise.all([api<RunListItem[]>('/tasks/activity'), api<RunListItem[]>('/runs?limit=100'), api<Task[]>('/tasks')])
    if (disposed)
      return
    latestRuns.value = runs
    recentRuns.value = recent
    state.tasks = tasks
    error.value = ''
  }
  catch (e) {
    if (!disposed)
      error.value = (e as Error).message
  }
  finally {
    refreshing = false
    loading.value = false
  }
}

onMounted(() => {
  wideQuery.addEventListener('change', onWide)
  void loadActivity()
  timer = setInterval(() => {
    now.value = Date.now()
    if (!document.hidden)
      void loadActivity()
  }, 5000)
})
onBeforeUnmount(() => {
  disposed = true
  clearInterval(timer)
  wideQuery.removeEventListener('change', onWide)
})
watch(() => route.query.new, (value) => {
  if (value !== '1')
    return
  editor.value = true
  void router.replace({ query: { ...route.query, new: undefined } })
}, { immediate: true })
const runColor = (status: RunListItem['status']) => status === 'succeeded' ? 'bg-success' : status === 'failed' || status === 'interrupted' ? 'bg-coral' : activeRun(status) ? 'bg-accent' : 'bg-line'
</script>

<template>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <div class="missions flex min-h-0 flex-1">
    <section class="flex min-h-0 w-105 shrink-0 flex-col tablet:w-full" aria-label="Mission list">
      <header class="flex items-start gap-2 pr-1">
        <div class="min-w-0 flex-1">
          <h1 class="m-0! font-heading text-[32px] leading-9 font-bold tracking-[-0.8px]">
            Missions
          </h1>
          <p class="m-0! mt-0.5! truncate text-base text-muted">
            {{ summary }}
          </p>
        </div>
        <button
          class="grid size-11 shrink-0 place-items-center rounded-full border border-line hover:bg-hover"
          aria-label="Search missions"
          :aria-pressed="searching || !!query"
          @click="toggleSearch"
        >
          <Icon :name="Search" :size="18" />
        </button>
        <button
          class="grid size-11 shrink-0 place-items-center rounded-full bg-ink text-canvas hover:opacity-90 disabled:opacity-40"
          aria-label="New mission"
          :disabled="!state.agents.length"
          @click="editor = true"
        >
          <Icon :name="Plus" :size="20" />
        </button>
      </header>
      <label v-if="searching || query" class="search-field mt-3 flex flex-row! min-h-11 items-center gap-2 rounded-full border border-line bg-surface px-4">
        <Icon :name="Search" :size="16" class="text-muted" /><input
          ref="searchInput"
          v-model="query"
          class="w-full! min-w-0 flex-1 outline-none"
          placeholder="Search missions"
          aria-label="Search missions"
        >
      </label>
      <div class="-mx-1 mt-2 flex shrink-0 gap-2 overflow-x-auto px-1 py-1.5 [scrollbar-width:none]" role="group" aria-label="Filter missions">
        <button
          v-for="option in missionFilters"
          :key="option.value"
          class="flex h-9 shrink-0 items-center gap-1.75 rounded-full border px-3.5 text-sm font-semibold"
          :class="filter === option.value ? 'border-ink bg-ink text-canvas' : 'border-line text-ink hover:bg-hover'"
          :aria-pressed="filter === option.value"
          @click="filter = option.value"
        >
          {{ option.label }}<span :class="filter === option.value ? 'text-canvas/60' : 'text-muted'">{{ state.tasks.filter(task => missionFilter(task, option.value)).length }}</span>
        </button>
      </div>
      <p v-if="loading" class="my-2! text-xs text-muted" role="status">
        Loading missions…
      </p>
      <ul class="m-0! mt-2! flex min-h-0 flex-1 list-none flex-col gap-2.5 overflow-y-auto overscroll-contain p-0! pb-6! [scrollbar-width:thin]" data-testid="missions">
        <li v-for="task in visible" :key="task.id">
          <article class="mission-card relative flex items-center gap-2.5 rounded-[22px] border py-3.5 pr-3 pl-4" :class="shown?.id === task.id && wide ? 'border-accent/30 bg-soft' : quiet(task) ? 'border-line bg-transparent' : 'border-line bg-surface'">
            <button
              class="absolute inset-0 rounded-[22px] focus-visible:outline-2 focus-visible:outline-accent"
              :aria-label="`Open ${task.name}`"
              :aria-current="shown?.id === task.id && wide ? 'true' : undefined"
              @click="select(task)"
            />
            <div class="pointer-events-none min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <AgentAvatar :name="state.agents.find(agent => agent.id === task.agentId)?.name ?? '?'" :identity="task.agentId" :size="22" />
                <ProjectLabel :name="state.projects.find(project => project.id === task.projectId)?.name ?? 'All projects'" :identity="task.projectId" />
              </div>
              <p class="mt-2! mb-0! line-clamp-2 font-heading text-base font-semibold" :class="quiet(task) ? 'text-muted' : 'text-ink'">
                {{ task.name }}
              </p>
              <p class="m-0! mt-1! flex min-w-0 items-center gap-1.25 text-xs text-muted">
                <Icon :name="quiet(task) ? Pause : Clock" :size="14" class="shrink-0" /><span class="truncate">{{ describeSchedule(task) }}</span>
                <span v-if="task.nextRun && task.enabled && !task.archived && task.cron" class="shrink-0 font-semibold text-ink">· {{ upcomingStamp(task.nextRun, now).split(' · ')[0] }}</span>
              </p>
              <p v-if="missionGroup(latest.get(task.id)) === 'review'" class="m-0! mt-1.5! text-xs font-semibold text-coral">
                {{ latest.get(task.id)?.outcome?.status === 'needs_input' ? 'Your input needed' : latest.get(task.id)?.outcome?.status === 'blocked' ? 'Blocked' : latest.get(task.id)?.status === 'interrupted' ? 'Interrupted' : 'Failed' }}
              </p>
              <p v-if="history.get(task.id)?.length" class="m-0! mt-2.5! flex gap-0.75" :aria-label="`${history.get(task.id)!.slice(0, 12).filter(item => item.status === 'succeeded').length} of the last ${history.get(task.id)!.slice(0, 12).length} runs succeeded`">
                <span
                  v-for="item in history.get(task.id)!.slice(0, 12).reverse()"
                  :key="item.id"
                  class="h-3.25 w-2 rounded-[3px]"
                  :class="runColor(item.status)"
                />
              </p>
            </div>
            <span
              v-if="activeRun(latest.get(task.id)?.status)"
              class="relative grid size-13 shrink-0 place-items-center rounded-full bg-accent text-xs font-bold text-surface"
              role="img"
              aria-label="Running"
            >
              {{ latest.get(task.id)?.status === 'queued' ? '…' : elapsedMinutes(latest.get(task.id)?.startedAt ?? null, now) }}
            </span>
            <button
              v-else
              class="relative grid size-13 shrink-0 place-items-center rounded-full disabled:opacity-40"
              :class="quiet(task) ? 'border border-line text-muted' : 'bg-ink text-canvas hover:opacity-90'"
              :aria-label="`Run ${task.name}`"
              :disabled="!!busy || task.archived"
              @click="run(task)"
            >
              <Icon :name="Play" :size="18" />
            </button>
          </article>
        </li>
        <li v-if="!loading && !visible.length">
          <Empty :title="state.tasks.length ? 'No matching missions' : 'No missions yet'" :description="state.tasks.length ? 'Change the search or the filters.' : 'Create a mission and hand it to an agent.'">
            <UiButton v-if="!state.tasks.length" @click="editor = true">
              <Icon :name="Plus" :size="16" />Create a mission
            </UiButton>
          </Empty>
        </li>
      </ul>
    </section>
    <section v-if="wide" class="task-focus-detail min-h-0 min-w-0 flex-1 overflow-y-auto border-l border-line pl-6 [scrollbar-width:thin]" aria-label="Selected mission">
      <MissionDetail
        v-if="shown"
        :key="shown.id"
        :task="shown"
        :latest="latest.get(shown.id)"
        :busy="busy === shown.id"
        @run="run(shown)"
        @edit="editor = shown"
        @pause="pause(shown)"
        @duplicate="duplicate(shown)"
        @archive="archive(shown)"
        @remove="deleting = shown"
      />
      <Empty v-else-if="!loading" title="Your missions, in one place" description="Create a mission to get started." />
    </section>
  </div>
  <Modal
    v-if="!wide && chosen"
    :title="chosen.name"
    sheet
    headless
    @close="closeSheet"
  >
    <div class="px-5 pt-5 pb-6">
      <MissionDetail
        :key="chosen.id"
        :task="chosen"
        :latest="latest.get(chosen.id)"
        :busy="busy === chosen.id"
        sheet
        @run="run(chosen)"
        @edit="editor = chosen"
        @pause="pause(chosen)"
        @duplicate="duplicate(chosen)"
        @archive="archive(chosen)"
        @remove="deleting = chosen"
        @close="closeSheet"
      />
    </div>
  </Modal>
  <TaskEditor
    v-if="editor"
    :task="editor === true ? undefined : editor"
    @saved="saved"
    @close="editor = null"
  />
  <Modal v-if="deleting" title="Delete this mission?" @close="deleting = null">
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <p>
        “{{ deleting.name }}” will stop scheduling. Its existing run history
        will remain available.
      </p>
    </div>
    <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
      <UiButton @click="deleting = null">
        Keep mission
      </UiButton><UiButton variant="danger" @click="remove">
        Delete mission
      </UiButton>
    </footer>
  </Modal>
</template>
