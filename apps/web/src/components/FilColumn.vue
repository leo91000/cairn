<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import type { FilItem } from '../signal'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { runMission as queueMission } from '../../../../packages/contracts/generated/runMission'
import { setMissionState } from '../../../../packages/contracts/generated/setMissionState'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import { waitLeft } from '../chat-wait'
import { setAside } from '../dismissals'
import { activeFilView } from '../fil-view'
import {
  Archive,
  Check,
  CheckCheck,
  Clock,
  CornerDownRight,
  ListFilter,
  MoreHorizontal,
  RotateCcw,
  X,
} from '../icons'
import { refreshMissionRuns, useMissionRuns } from '../mission-runs'
import { localReads, markRead } from '../reads'
import { typingTarget } from '../shortcuts'
import {
  dismissible,
  filOf,
  filViews,
  isUnread,
  shortAge,
  tomorrowMorning,
} from '../signal'
import HarnessMark from './HarnessMark.vue'
import Icon from './Icon.vue'
import InstallationHeader from './InstallationHeader.vue'
import MenuButton from './MenuButton.vue'

const props = withDefaults(defineProps<{
  chats: ChatView[]
  selected?: string | null
  selectedRun?: string | null
  compact?: boolean
}>(), { selected: null, selectedRun: null, compact: false })
const router = useRouter()
const runs = useMissionRuns()
const filter = ref('')
const filterInput = ref<HTMLInputElement>()
const cursor = ref<string | null>(null)
const tab = activeFilView
const now = ref(Date.now())
const clock = setInterval(() => now.value = Date.now(), 30000)
onBeforeUnmount(() => clearInterval(clock))
const views = computed(() => filViews(filOf(props.chats, state.tasks, runs.value, localReads, now.value)))
const tabs = computed(() => [
  { key: 'unread' as const, label: 'À lire', count: views.value.unread.length },
  { key: 'ongoing' as const, label: 'En cours', count: views.value.ongoing.length },
  { key: 'all' as const, label: 'Tout', count: null },
])
const flat = computed(() => views.value[tab.value].filter((item) => {
  const query = filter.value.trim().toLowerCase()
  return !query || [item.title, item.subtitle, item.agent, item.project ?? ''].some(value => value.toLowerCase().includes(query))
}))
const opened = (item: FilItem) => (item.chatId && item.chatId === props.selected) || (item.runId && item.runId === props.selectedRun)
const current = (item: FilItem) => opened(item) || cursor.value === item.key
watch(() => [props.selected, props.selectedRun], () => {
  cursor.value = views.value.all.find(opened)?.key ?? null
}, { immediate: true })

function tone(item: FilItem) {
  if (item.kind === 'question' || (item.kind === 'review' && item.outcome === 'needs_input'))
    return 'text-answer'
  if (item.kind === 'failed' || item.kind === 'failed-mission')
    return 'text-coral'
  if (item.kind === 'review')
    return 'text-warning'
  if (['running', 'queued', 'waiting', 'snoozed', 'suspended'].includes(item.kind))
    return 'text-accent'
  return 'text-muted'
}

function label(item: FilItem) {
  if (item.kind === 'question' || (item.kind === 'review' && item.outcome === 'needs_input'))
    return 'Answer'
  if (item.kind === 'unread')
    return item.taskId ? 'Mission finished' : 'Finished'
  if (item.kind === 'failed-mission')
    return 'Mission failed'
  if (item.kind === 'review')
    return 'Blocked'
  return item.subtitle
}

function open(item: FilItem) {
  cursor.value = item.key
  // Only the synced detail acknowledges the work once it has actually been displayed.
  void router.push(item.to)
}

function read(item: FilItem) {
  if (item.readAt)
    void markRead([{ key: item.key, at: item.readAt }])
}

function readAll() {
  void markRead(views.value.unread.filter(item => item.readAt).map(item => ({ key: item.key, at: item.readAt! })))
}

async function runAgain(item: FilItem) {
  try {
    await queueMission(api, { taskId: item.taskId! })
    notify(`${item.title} started`)
    await refreshMissionRuns()
  }
  catch (error) {
    notify((error as Error).message)
  }
}

function dismiss(item: FilItem) {
  void setAside({ id: item.runId!, taskId: item.taskId!, finishedAt: item.at })
}

function snooze(item: FilItem) {
  void setAside({ id: item.runId!, taskId: item.taskId!, finishedAt: item.at }, tomorrowMorning(now.value))
}

async function archive(item: FilItem) {
  const task = state.tasks.find(task => task.id === item.taskId)
  if (!task)
    return
  try {
    await setMissionState(api, {
      taskId: task.id,
      revision: task.revision,
      archived: true,
      enabled: false,
    })
    await refresh()
    notify(`${item.title} archived`)
  }
  catch (error) {
    notify((error as Error).message)
  }
}

function move(delta: number) {
  const items = flat.value
  if (!items.length)
    return
  const index = items.findIndex(item => item.key === cursor.value)
  const next = items[Math.min(items.length - 1, Math.max(0, index < 0 ? 0 : index + delta))]
  cursor.value = next.key
  document.querySelector(`[data-fil-key="${CSS.escape(next.key)}"]`)?.scrollIntoView({ block: 'nearest' })
}

function key(event: KeyboardEvent) {
  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || typingTarget(event.target) || document.querySelector('dialog[open], [popover]:popover-open'))
    return
  if (event.key === 'j' || event.key === 'k') {
    event.preventDefault()
    move(event.key === 'j' ? 1 : -1)
  }
  else if (event.key === 'Enter' && cursor.value && !(event.target instanceof HTMLElement && event.target.closest('a, button, input, textarea, select, summary, [tabindex]'))) {
    const item = flat.value.find(item => item.key === cursor.value)
    if (item) {
      event.preventDefault()
      open(item)
    }
  }
  else if (event.key === 'e') {
    const item = flat.value.find(item => item.key === cursor.value)
    if (item && isUnread(item)) {
      event.preventDefault()
      read(item)
    }
  }
  else if (event.key === '/') {
    event.preventDefault()
    filterInput.value?.focus()
  }
}

function tabKey(event: KeyboardEvent, index: number) {
  const delta = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0
  const next = event.key === 'Home' ? 0 : event.key === 'End' ? 2 : delta ? (index + delta + 3) % 3 : null
  if (next === null)
    return
  event.preventDefault()
  tab.value = tabs.value[next].key
  ;(event.currentTarget as HTMLElement).parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus()
}

onMounted(() => document.addEventListener('keydown', key))
onBeforeUnmount(() => document.removeEventListener('keydown', key))
</script>

<template>
  <div class="fil flex h-full min-h-0 flex-col">
    <header class="shrink-0 px-5 pb-2" :class="compact ? 'pt-5' : 'pt-6 phone:px-4 phone:pt-[max(10px,env(safe-area-inset-top))]'">
      <InstallationHeader v-if="!compact" class="mb-4" />
      <div class="flex items-center gap-2">
        <h1 class="font-heading text-lg! font-bold! text-ink">
          Fil
        </h1>
        <span v-if="views.unread.length" class="rounded-md bg-soft px-1.5 text-[11px] font-semibold tabular-nums text-accent">{{ views.unread.length }}</span>
        <button
          v-if="views.unread.length"
          type="button"
          class="ml-auto rounded-lg px-2 py-1 text-[11px] text-muted hover:bg-hover hover:text-ink"
          aria-label="Mark all as read"
          @click="readAll"
        >
          <Icon :name="CheckCheck" :size="14" />
        </button>
      </div>
      <label class="fil-filter mt-4 flex h-9 flex-row! items-center gap-2 rounded-lg border border-line bg-surface px-3 text-muted focus-within:border-accent">
        <Icon :name="ListFilter" :size="15" />
        <input
          ref="filterInput"
          v-model="filter"
          class="min-h-0! min-w-0 flex-1 border-0! bg-transparent! p-0! text-[12px] text-ink shadow-none! outline-none"
          placeholder="Filter the Fil"
          aria-label="Filter conversations"
          @keydown.esc.stop="filter = ''; filterInput?.blur()"
        >
        <kbd v-if="!filter" class="keycap phone:hidden">/</kbd>
        <button
          v-else
          type="button"
          class="grid size-6 place-items-center rounded-md hover:bg-hover"
          aria-label="Clear filter"
          @click="filter = ''"
        ><Icon :name="X" :size="14" /></button>
      </label>
      <div role="tablist" aria-label="Fil views" class="mt-3 flex gap-5 border-b border-line">
        <button
          v-for="(view, index) in tabs"
          :key="view.key"
          type="button"
          role="tab"
          :aria-selected="tab === view.key"
          :tabindex="tab === view.key ? 0 : -1"
          class="-mb-px flex items-center gap-1.5 border-b-2 px-0 pb-2.5 pt-1 text-[12px] font-medium"
          :class="tab === view.key ? 'border-accent text-ink' : 'border-transparent text-muted hover:text-ink'"
          @click="tab = view.key"
          @keydown="tabKey($event, index)"
        >
          {{ view.label }}<span v-if="view.count" class="text-[10px] tabular-nums text-subtle">{{ view.count }}</span>
        </button>
      </div>
    </header>
    <div class="min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 pb-10 [scrollbar-width:thin] phone:pb-32" role="region" :aria-label="tabs.find(view => view.key === tab)?.label">
      <p v-if="!flat.length" class="px-3 py-10 text-center text-[13px] text-muted">
        {{ filter ? `Nothing matches “${filter}”.` : tab === 'unread' ? 'All read. Your tasks are in En cours and Tout.' : tab === 'ongoing' ? 'No work in progress.' : 'No conversations yet.' }}
      </p>
      <article
        v-for="item in flat"
        :key="item.key"
        :data-fil-key="item.key"
        :data-fil-kind="item.kind"
        :data-unread="isUnread(item) ? '' : undefined"
        class="group relative flex items-center border-b border-line/60"
        :class="current(item) ? 'bg-hover' : 'hover:bg-hover/70'"
      >
        <button
          type="button"
          class="flex min-h-16 min-w-0 flex-1 items-start gap-2 px-3 py-3 text-left"
          :aria-label="`Open “${item.title}”`"
          :aria-current="opened(item) ? 'page' : undefined"
          @click="open(item)"
        >
          <span class="mt-1.5 w-1.5 shrink-0"><span
            v-if="isUnread(item)"
            class="block size-1.5 rounded-full bg-accent"
            role="img"
            aria-label="Unread"
          /></span>
          <span class="min-w-0 flex-1">
            <span class="flex items-start gap-2"><span class="line-clamp-2 text-[13px] leading-snug text-ink" :class="isUnread(item) ? 'font-semibold' : 'font-medium'">{{ item.title }}</span><span class="ml-auto shrink-0 text-[10px] tabular-nums text-subtle">{{ shortAge(item.at, now) }}</span></span>
            <span class="mt-1 flex min-w-0 items-center gap-1.5 text-[11px]">
              <span class="truncate" :class="tone(item)">{{ label(item) }}</span><span class="text-subtle">·</span><HarnessMark v-if="item.harness" :provider="item.harness" /><span class="truncate text-muted">{{ item.project || item.agent }}</span>
            </span>
            <span v-if="item.requester" class="mt-1 flex items-center gap-1 truncate text-[10px] text-subtle" data-testid="fil-requester"><Icon :name="CornerDownRight" :size="10" />Delegated by {{ item.requester.title }}</span>
            <span v-if="item.wakeAt !== undefined" class="mt-1 block text-[10px] text-accent">{{ waitLeft({ wakeAt: item.wakeAt, condition: item.condition, resolved: item.resolved }, now) }}</span>
          </span>
        </button>
        <MenuButton :label="`More actions for “${item.title}”`" trigger-class="mr-1 grid size-7 shrink-0 place-items-center rounded-md text-muted opacity-0 group-hover:opacity-100 focus:opacity-100 phone:opacity-100 hover:bg-hover" :width="210">
          <template #trigger>
            <Icon :name="MoreHorizontal" :size="15" />
          </template>
          <template #default="{ select }">
            <button
              v-if="isUnread(item) && item.readAt"
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => read(item))"
            >
              <Icon :name="Check" :size="16" />Mark as read
            </button>
            <button
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => open(item))"
            >
              Open conversation
            </button>
            <button
              v-if="dismissible(item)"
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => dismiss(item))"
            >
              <Icon :name="Check" :size="16" />Dismiss outcome
            </button>
            <button
              v-if="item.kind === 'review'"
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => snooze(item))"
            >
              <Icon :name="Clock" :size="16" />Remind me tomorrow
            </button>
            <button
              v-if="item.taskId"
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => runAgain(item))"
            >
              <Icon :name="RotateCcw" :size="16" />Run again
            </button>
            <button
              v-if="item.taskId"
              type="button"
              role="menuitem"
              tabindex="-1"
              class="menu-item"
              @click="select(() => archive(item))"
            >
              <Icon :name="Archive" :size="16" />Archive mission
            </button>
          </template>
        </MenuButton>
      </article>
    </div>
  </div>
</template>
