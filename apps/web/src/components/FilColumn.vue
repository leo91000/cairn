<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import type { FilEntry, FilItem } from '../signal'
import {
  computed,
  nextTick,
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
  ChevronDown,
  Circle,
  CircleAlert,
  CirclePause,
  CircleX,
  Clock,
  CornerDownRight,
  Hourglass,
  ListFilter,
  MoreHorizontal,
  RefreshCw,
  RotateCcw,
  Search,
  X,
} from '../icons'
import { refreshMissionRuns, useMissionRuns } from '../mission-runs'
import { localReads, markRead } from '../reads'
import { typingTarget } from '../shortcuts'
import {
  dismissible,
  filFolds,
  filOf,
  filThread,
  filViews,
  isUnread,
  shortAge,
  tomorrowMorning,
} from '../signal'
import AgentAvatar from './AgentAvatar.vue'
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
const rows = computed(() => filThread(views.value[tab.value].filter((item) => {
  const query = filter.value.trim().toLowerCase()
  return !query || [item.title, item.subtitle, item.agent, item.project ?? ''].some(value => value.toLowerCase().includes(query))
})))
const opened = (item: FilItem) => (item.chatId && item.chatId === props.selected) || (item.runId && item.runId === props.selectedRun)
const current = (item: FilItem) => opened(item) || cursor.value === item.key
// Quiet missions fold together, except while filtering, which looks for each row.
const unfolded = ref(new Set<string>())
const lines = computed(() => (filter.value.trim() ? rows.value as FilEntry[] : filFolds(rows.value)).flatMap((entry) => {
  if (!entry.folded) {
    return [{
      key: entry.item.key,
      item: entry.item,
      nested: entry.nested,
      folded: undefined,
      open: false,
    }]
  }

  const key = `fold:${entry.item.key}`
  // The work open beside the Fil stays visible in its fold.
  const open = unfolded.value.has(key) || entry.folded.some(opened)
  return [
    {
      key,
      item: entry.item,
      nested: false,
      folded: entry.folded,
      open,
    },
    ...(open
      ? entry.folded.map(item => ({
          key: item.key,
          item,
          nested: true,
          folded: undefined,
          open: false,
        }))
      : []),
  ]
}))
const flat = computed(() => lines.value.filter(line => !line.folded).map(line => line.item))

// A finished result's opening, beside the Fil: reading it there neither opens nor reads the work.
const peeked = ref<{ item: FilItem, top: number, left: number } | null>(null)
let peekTimer: ReturnType<typeof setTimeout> | undefined
const hoverable = typeof matchMedia === 'function' && matchMedia('(hover: hover)').matches
onBeforeUnmount(() => clearTimeout(peekTimer))

function peek(item: FilItem, event: Event) {
  clearTimeout(peekTimer)
  if (!item.preview || !hoverable)
    return
  const row = (event.currentTarget as HTMLElement).closest('article')!.getBoundingClientRect()
  const width = 320
  const beside = row.right + 12 + width <= innerWidth - 8
  peekTimer = setTimeout(() => {
    peeked.value = {
      item,
      left: beside ? row.right + 12 : Math.max(8, row.right - width),
      top: Math.max(8, Math.min(beside ? row.top : row.bottom + 4, innerHeight - 240)),
    }
  }, 350)
}

function unpeek() {
  clearTimeout(peekTimer)
  peeked.value = null
}

function toggleFold(key: string) {
  const next = new Set(unfolded.value)
  if (!next.delete(key))
    next.add(key)
  unfolded.value = next
}

const tabIndex = computed(() => tabs.value.findIndex(view => view.key === tab.value))
// The agents at work, once each, for the header's stack of avatars.
const working = computed(() => [...new Map(views.value.all
  .filter(item => item.kind === 'running')
  .map(item => [item.agentKey || item.agent, item])).values()])
// The filter opens on demand and stays open while it holds a query.
const searchOpen = ref(false)
const searching = computed(() => searchOpen.value || !!filter.value)

async function openSearch() {
  searchOpen.value = true
  await nextTick()
  filterInput.value?.focus()
}

function closeSearch() {
  filter.value = ''
  searchOpen.value = false
  filterInput.value?.blur()
}

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

// The corner of the agent's avatar: what the row asks for, while finished work stays quiet.
function mark(item: FilItem) {
  if (item.kind === 'question' || (item.kind === 'review' && item.outcome === 'needs_input'))
    return CircleAlert
  if (item.kind === 'failed' || item.kind === 'failed-mission')
    return CircleX
  if (item.kind === 'review' || item.kind === 'paused')
    return CirclePause
  if (['queued', 'waiting', 'snoozed', 'suspended'].includes(item.kind))
    return Clock
  if (item.kind === 'set-aside')
    return Hourglass
  return Circle
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
  unpeek()
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
    void openSearch()
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
  <div class="fil flex h-full min-h-0 flex-col [--fil-bg:var(--color-inset)] tablet:[--fil-bg:var(--color-canvas)]">
    <header class="shrink-0 px-5 pb-2" :class="compact ? 'pt-5' : 'pt-6 phone:px-4 phone:pt-[max(10px,env(safe-area-inset-top))]'">
      <InstallationHeader v-if="!compact" class="mb-4" />
      <div class="flex h-9 items-center gap-2">
        <h1 class="font-heading text-xl! font-bold! tracking-tight text-ink">
          Fil
        </h1>
        <button
          v-if="working.length"
          type="button"
          class="ml-1 flex items-center gap-1.5 rounded-full bg-soft py-0.5 pl-0.5 pr-2.5 text-[11px] font-medium text-accent transition-colors hover:bg-soft/70"
          data-testid="fil-at-work"
          @click="tab = 'ongoing'"
        >
          <span class="flex -space-x-1">
            <span v-for="item in working.slice(0, 3)" :key="item.key" class="rounded-[7px] ring-2 ring-soft">
              <AgentAvatar :name="item.agent" :identity="item.agentKey" :size="18" />
            </span>
          </span>
          {{ working.length }} at work
        </button>
        <div class="ml-auto flex items-center gap-0.5">
          <button
            type="button"
            class="grid size-8 place-items-center rounded-lg transition-colors hover:bg-hover hover:text-ink"
            :class="searching ? 'bg-hover text-ink' : 'text-muted'"
            aria-label="Filter the Fil"
            title="Filter the Fil (/)"
            :aria-expanded="searching"
            aria-controls="fil-search"
            @mousedown.prevent
            @click="searching ? closeSearch() : openSearch()"
          >
            <Icon :name="Search" :size="16" />
          </button>
          <button
            v-if="views.unread.length"
            type="button"
            class="grid size-8 place-items-center rounded-lg text-muted transition-colors hover:bg-hover hover:text-ink"
            aria-label="Mark all as read"
            title="Mark all as read"
            @click="readAll"
          >
            <Icon :name="CheckCheck" :size="16" />
          </button>
        </div>
      </div>
      <div
        id="fil-search"
        class="fil-search grid transition-[grid-template-rows,opacity,margin] duration-200 ease-out motion-reduce:transition-none"
        :class="searching ? 'mt-3 grid-rows-[1fr] opacity-100' : 'mt-0 grid-rows-[0fr] opacity-0'"
        :inert="!searching"
      >
        <div class="min-h-0 overflow-hidden">
          <label
            class="fil-filter flex h-9 flex-row! items-center gap-2 rounded-full border border-line bg-surface px-3.5 text-muted transition-transform duration-200 ease-out focus-within:border-accent motion-reduce:transition-none"
            :class="searching ? 'translate-y-0 scale-100' : '-translate-y-2 scale-[0.97]'"
          >
            <Icon :name="ListFilter" :size="15" />
            <input
              ref="filterInput"
              v-model="filter"
              class="min-h-0! min-w-0 flex-1 border-0! bg-transparent! p-0! text-[12px] text-ink shadow-none! outline-none"
              placeholder="Title, agent or project"
              aria-label="Filter conversations"
              @keydown.esc.stop="closeSearch"
              @blur="searchOpen = !!filter"
            >
            <button
              type="button"
              class="grid size-6 place-items-center rounded-full hover:bg-hover"
              aria-label="Close the filter"
              @mousedown.prevent
              @click="closeSearch"
            ><Icon :name="X" :size="14" /></button>
          </label>
        </div>
      </div>
      <div role="tablist" aria-label="Fil views" class="relative mt-3 grid grid-cols-3 rounded-xl bg-variant/60 p-1">
        <span
          aria-hidden="true"
          class="absolute inset-y-1 left-1 w-[calc((100%-8px)/3)] rounded-lg bg-surface shadow-[0_1px_2px_#0000001f] ring-1 ring-line/70 transition-transform duration-200 ease-out motion-reduce:transition-none"
          :style="{ transform: `translateX(${tabIndex * 100}%)` }"
        />
        <button
          v-for="(view, index) in tabs"
          :key="view.key"
          type="button"
          role="tab"
          :aria-selected="tab === view.key"
          :tabindex="tab === view.key ? 0 : -1"
          class="relative flex h-7 items-center justify-center gap-1.5 rounded-lg text-[12px] font-medium transition-colors"
          :class="tab === view.key ? 'text-ink' : 'text-muted hover:text-ink'"
          @click="tab = view.key"
          @keydown="tabKey($event, index)"
        >
          {{ view.label }}<span v-if="view.count" class="min-w-4 rounded-full px-1 text-center text-[10px] font-semibold leading-4 tabular-nums" :class="view.key === 'unread' ? 'bg-accent text-surface' : 'bg-line text-muted'">{{ view.count }}</span>
        </button>
      </div>
    </header>
    <div
      class="min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 pb-10 [scrollbar-width:thin] phone:pb-32"
      role="region"
      :aria-label="tabs.find(view => view.key === tab)?.label"
      @scroll.passive="unpeek"
    >
      <p v-if="!flat.length" class="px-3 py-10 text-center text-[13px] text-muted">
        {{ filter ? `Nothing matches “${filter}”.` : tab === 'unread' ? 'All read. Your tasks are in En cours and Tout.' : tab === 'ongoing' ? 'No work in progress.' : 'No conversations yet.' }}
      </p>
      <template v-for="{ key: lineKey, item, nested, folded, open: unfoldedLine } in lines" :key="lineKey">
        <button
          v-if="folded"
          type="button"
          class="flex w-full items-start gap-2 rounded-lg py-2.5 pr-2 pl-1.5 text-left transition-colors hover:bg-hover/70"
          :aria-expanded="unfoldedLine"
          :aria-label="`${folded.length} missions finished: ${folded.map(entry => entry.title).join(', ')}`"
          :data-fil-fold="lineKey"
          @click="toggleFold(lineKey)"
        >
          <span class="w-1.5 shrink-0" />
          <span class="relative mt-0.5 size-[26px] shrink-0">
            <span class="absolute top-0 left-0 opacity-80"><AgentAvatar :name="folded[1].agent" :identity="folded[1].agentKey" :size="18" /></span>
            <span class="absolute right-0 bottom-0 rounded-[7px] ring-2 ring-(--fil-bg)"><AgentAvatar :name="folded[0].agent" :identity="folded[0].agentKey" :size="18" /></span>
          </span>
          <span class="min-w-0 flex-1">
            <span class="flex items-baseline gap-2"><span class="truncate text-[13px] leading-snug font-medium text-ink/85">{{ folded.length }} missions finished</span><span class="ml-auto shrink-0 text-[10px] text-subtle tabular-nums">{{ shortAge(item.at, now) }}</span></span>
            <span class="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] text-subtle">
              <Icon :name="RefreshCw" :size="10" /><span class="truncate">{{ folded.map(entry => entry.title).join(' · ') }}</span>
              <Icon
                :name="ChevronDown"
                :size="13"
                class="ml-auto shrink-0 transition-transform duration-200 motion-reduce:transition-none"
                :class="unfoldedLine && 'rotate-180'"
              />
            </span>
          </span>
        </button>
        <article
          v-else
          :data-fil-key="item.key"
          :data-fil-kind="item.kind"
          :data-unread="isUnread(item) ? '' : undefined"
          :data-fil-nested="nested ? '' : undefined"
          class="group relative flex items-center rounded-lg"
          :class="[current(item) ? 'bg-hover' : 'hover:bg-hover/70', nested && 'ml-[33px] rounded-l-none border-l border-line']"
        >
          <button
            type="button"
            class="flex min-w-0 flex-1 items-start gap-2 py-2.5 pr-2 text-left"
            :class="nested ? 'pl-2.5' : 'pl-1.5'"
            :aria-label="`Open “${item.title}”`"
            :aria-description="item.finished ? label(item) : undefined"
            :aria-current="opened(item) ? 'page' : undefined"
            :aria-describedby="peeked?.item.key === item.key ? 'fil-peek' : undefined"
            @click="open(item)"
            @mouseenter="peek(item, $event)"
            @mouseleave="unpeek"
            @focus="peek(item, $event)"
            @blur="unpeek"
          >
            <span class="mt-[11px] w-1.5 shrink-0"><span
              v-if="isUnread(item)"
              class="block size-1.5 rounded-full bg-accent"
              role="img"
              aria-label="Unread"
            /></span>
            <span class="relative mt-0.5 shrink-0">
              <AgentAvatar
                :name="item.agent"
                :identity="item.agentKey"
                :size="nested ? 20 : 26"
                :working="item.kind === 'running'"
              />
              <span
                v-if="!item.finished && item.kind !== 'running'"
                class="absolute -right-1.5 -bottom-1 grid size-4 place-items-center rounded-full bg-(--fil-bg)"
                data-testid="fil-mark"
              ><Icon :name="mark(item)" :size="13" :class="tone(item) === 'text-muted' ? 'text-subtle' : tone(item)" /></span>
            </span>
            <span class="min-w-0 flex-1">
              <span class="flex items-baseline gap-2"><span class="truncate text-[13px] leading-snug" :class="[isUnread(item) ? 'font-semibold text-ink' : 'font-medium', !isUnread(item) && (nested ? 'text-muted' : 'text-ink/85')]">{{ item.title }}</span><span class="ml-auto shrink-0 text-[10px] tabular-nums text-subtle group-hover:invisible group-has-[[aria-expanded=true]]:invisible phone:visible!">{{ shortAge(item.at, now) }}</span></span>
              <span v-if="!nested || !item.finished" class="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] text-subtle">
                <span :class="item.finished ? 'sr-only' : ['truncate font-medium', tone(item)]">{{ label(item) }}</span><span v-if="!item.finished && !nested">·</span><template v-if="!nested"><HarnessMark v-if="item.harness" :provider="item.harness" /><Icon v-if="item.taskId" :name="RefreshCw" :size="10" /><span class="truncate">{{ item.project || item.agent }}</span></template>
              </span>
              <span v-if="item.requester && !nested" class="mt-0.5 flex items-center gap-1 truncate text-[10px] text-subtle" data-testid="fil-requester"><Icon :name="CornerDownRight" :size="10" />Delegated by {{ item.requester.title }}</span>
              <span v-if="item.wakeAt !== undefined" class="mt-0.5 block text-[10px] text-accent">{{ waitLeft({ wakeAt: item.wakeAt, condition: item.condition, resolved: item.resolved }, now) }}</span>
            </span>
          </button>
          <MenuButton :label="`More actions for “${item.title}”`" trigger-class="absolute right-1 top-1.5 grid size-7 shrink-0 place-items-center rounded-md bg-hover text-muted opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-ink aria-expanded:opacity-100 phone:static phone:mr-1 phone:bg-transparent phone:opacity-100" :width="210">
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
      </template>
    </div>
    <Teleport to="body">
      <Transition
        enter-active-class="transition duration-150 ease-out motion-reduce:transition-none"
        enter-from-class="opacity-0 -translate-x-1"
        leave-active-class="transition duration-100 ease-in motion-reduce:transition-none"
        leave-to-class="opacity-0"
      >
        <div
          v-if="peeked"
          id="fil-peek"
          role="tooltip"
          class="pointer-events-none fixed z-40 w-80 rounded-xl border border-line bg-raised p-3.5 shadow-[0_12px_32px_#0000002e]"
          :style="{ top: `${peeked.top}px`, left: `${peeked.left}px` }"
        >
          <div class="flex items-center gap-2">
            <AgentAvatar :name="peeked.item.agent" :identity="peeked.item.agentKey" :size="20" />
            <span class="truncate text-[12px] font-medium text-muted">{{ [peeked.item.agent, peeked.item.project].filter(Boolean).join(' · ') }}</span>
            <span class="ml-auto shrink-0 text-[10px] text-subtle tabular-nums">{{ shortAge(peeked.item.at, now) }}</span>
          </div>
          <p class="mt-2 line-clamp-2 text-[13px] font-semibold text-ink">
            {{ peeked.item.title }}
          </p>
          <p class="mt-1.5 line-clamp-6 text-[12.5px] leading-relaxed whitespace-pre-line text-muted">
            {{ peeked.item.preview }}
          </p>
          <p class="mt-2.5 text-[10.5px] text-subtle">
            Open it to read it all. It stays unread until then.
          </p>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>
