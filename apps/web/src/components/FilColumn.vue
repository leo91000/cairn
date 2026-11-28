<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import type { FilItem } from '../signal'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { state } from '../api'
import { waitLeft } from '../chat-wait'
import { restoreConversation, snoozeConversation } from '../dismissals'
import { activeFilView } from '../fil-view'
import {
  Check,
  CheckCheck,
  Circle,
  CircleAlert,
  CirclePause,
  CircleX,
  Clock,
  CornerDownRight,
  ListFilter,
  MoreHorizontal,
  Search,
  X,
} from '../icons'
import { localReads, markRead } from '../reads'
import { localDateTime } from '../schedule-time'
import { typingTarget } from '../shortcuts'
import {
  filOf,
  filThread,
  filViews,
  isUnread,
  shortAge,
  tomorrowMorning,
} from '../signal'
import { warmHistory } from '../use-live-run'
import AgentAvatar from './AgentAvatar.vue'
import HarnessMark from './HarnessMark.vue'
import Icon from './Icon.vue'
import InstallationHeader from './InstallationHeader.vue'
import MenuButton from './MenuButton.vue'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = withDefaults(defineProps<{
  chats: ChatView[]
  selected?: string | null
  compact?: boolean
}>(), { selected: null, compact: false })
const router = useRouter()
const filter = ref('')
const filterInput = ref<HTMLInputElement>()
const cursor = ref<string | null>(null)
const tab = activeFilView
const now = ref(Date.now())
const clock = setInterval(() => now.value = Date.now(), 30000)
onBeforeUnmount(() => clearInterval(clock))
const views = computed(() => filViews(filOf(props.chats, localReads, now.value, state.usableAgents)))
const tabs = computed(() => [
  { key: 'unread' as const, label: 'À lire', count: views.value.unread.length },
  { key: 'ongoing' as const, label: 'En cours', count: views.value.ongoing.length },
  { key: 'all' as const, label: 'Tout', count: null },
])
const rows = computed(() => filThread(views.value[tab.value].filter((item) => {
  const query = filter.value.trim().toLowerCase()
  return !query || [item.title, item.subtitle, item.agent, item.project ?? ''].some(value => value.toLowerCase().includes(query))
})))
const opened = (item: FilItem) => item.chatId === props.selected
const current = (item: FilItem) => opened(item) || cursor.value === item.key
const flat = computed(() => rows.value.map(row => row.item))

// A finished result's opening, beside the Fil: reading it there neither opens nor reads the work.
const peeked = ref<{ item: FilItem, top: number, left: number } | null>(null)
const peekCard = ref<HTMLElement>()
let peekTarget: HTMLElement | undefined
let peekTimer: ReturnType<typeof setTimeout> | undefined
let hidePeekTimer: ReturnType<typeof setTimeout> | undefined
const hoverable = typeof matchMedia === 'function' && matchMedia('(hover: hover)').matches
onBeforeUnmount(unpeek)

function peek(item: FilItem, event: Event) {
  const target = event.currentTarget as HTMLElement
  // Keyboard focus owns its preview until it moves or Escape dismisses it.
  if (event.type === 'mouseenter' && peekTarget !== target && peekTarget?.matches(':focus'))
    return
  if (!item.preview || !hoverable) {
    if (!peekTarget?.matches(':hover, :focus') && !peekCard.value?.matches(':hover'))
      unpeek()
    return
  }

  clearTimeout(peekTimer)
  keepPeek()
  peekTarget = target
  const row = peekTarget.closest('article')!.getBoundingClientRect()
  const width = 320
  const beside = row.right + 12 + width <= innerWidth - 8
  peekTimer = setTimeout(() => {
    peekTimer = undefined
    peeked.value = {
      item,
      left: beside ? row.right + 12 : Math.max(8, row.right - width),
      top: Math.max(8, Math.min(beside ? row.top : row.bottom + 4, innerHeight - 240)),
    }
  }, 350)
}

function unpeek() {
  clearTimeout(peekTimer)
  clearTimeout(hidePeekTimer)
  peekTimer = undefined
  hidePeekTimer = undefined
  peekTarget = undefined
  peeked.value = null
}

function keepPeek() {
  clearTimeout(hidePeekTimer)
  hidePeekTimer = undefined
}

function leavePeek() {
  // Preserve focus/hover ownership and allow the pointer to cross the gap to the card.
  if (peekTarget?.matches(':hover, :focus') || peekCard.value?.matches(':hover'))
    return
  clearTimeout(peekTimer)
  peekTimer = undefined
  keepPeek()
  hidePeekTimer = setTimeout(unpeek, 150)
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

watch(() => props.selected, () => {
  cursor.value = views.value.all.find(opened)?.key ?? null
}, { immediate: true })

function tone(item: FilItem) {
  if (item.kind === 'question')
    return 'text-answer'
  if (item.kind === 'failed')
    return 'text-coral'
  if (item.kind === 'review')
    return 'text-warning'
  if (['running', 'queued', 'waiting', 'snoozed', 'suspended'].includes(item.kind))
    return 'text-accent'
  return 'text-muted'
}

// The corner of the agent's avatar: what the row asks for, while finished work stays quiet.
function mark(item: FilItem) {
  if (item.kind === 'question')
    return CircleAlert
  if (item.kind === 'failed')
    return CircleX
  if (item.kind === 'review' || item.kind === 'paused')
    return CirclePause
  if (['queued', 'waiting', 'snoozed', 'suspended'].includes(item.kind))
    return Clock
  return Circle
}

function label(item: FilItem) {
  if (item.kind === 'question')
    return 'Answer'
  if (item.kind === 'review')
    return 'Blocked'
  return item.subtitle
}

// A conversation about to open is read from storage first, so it shows with the click. A
// pointer resting on a row reads it; one sweeping across the Fil does not.
let warmTimer: ReturnType<typeof setTimeout> | undefined

function warm(item: FilItem, delay = 0) {
  clearTimeout(warmTimer)
  if (!item.chatId || !item.to.startsWith('/chats/'))
    return
  const path = `${decodeURIComponent(item.to)}/stream`
  warmTimer = setTimeout(() => void warmHistory(path).catch(() => {}), delay)
}

function unwarm() {
  clearTimeout(warmTimer)
}

onBeforeUnmount(unwarm)

function open(item: FilItem) {
  unpeek()
  cursor.value = item.key
  // Only the synced detail acknowledges the work once it has actually been displayed.
  void router.push(item.to)
}

function read(item: FilItem) {
  if (item.readAt) {
    void markRead([{
      key: item.key,
      at: item.readAt,
      snoozedUntil: item.kind === 'snoozed' ? item.wakeAt : undefined,
    }])
  }
}

function readAll() {
  void markRead(views.value.unread.filter(item => item.readAt).map(item => ({ key: item.key, at: item.readAt! })))
}

const snoozing = ref<FilItem | null>(null)
const snoozeDate = ref('')
const snoozeDeadline = computed(() => new Date(snoozeDate.value).getTime())

function openSnooze(item: FilItem) {
  snoozeDate.value = localDateTime(tomorrowMorning())
  snoozing.value = item
}

function snooze() {
  const item = snoozing.value
  if (!item || snoozeDeadline.value <= Date.now() || !Number.isFinite(snoozeDeadline.value))
    return

  snoozing.value = null
  void snoozeConversation(item.chatId, item.snoozeAt ?? item.at, snoozeDeadline.value)
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
  if (!event.defaultPrevented && event.key === 'Escape' && (peeked.value || peekTimer)) {
    event.preventDefault()
    unpeek()
    return
  }

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
      <p v-if="!rows.length" class="px-3 py-10 text-center text-[13px] text-muted">
        {{ filter ? `Nothing matches “${filter}”.` : tab === 'unread' ? 'All read. Your conversations are in En cours and Tout.' : tab === 'ongoing' ? 'No work in progress.' : 'No conversations yet.' }}
      </p>
      <template v-for="{ item, nested } in rows" :key="item.key">
        <article
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
            @mouseleave="leavePeek"
            @pointerenter="warm(item, 120)"
            @pointerleave="unwarm"
            @pointerdown="warm(item)"
            @focus="peek(item, $event); warm(item)"
            @blur="leavePeek"
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
              <span class="flex items-baseline gap-2"><span class="truncate text-[13px] leading-snug" :class="[isUnread(item) ? 'font-semibold text-ink' : 'font-medium', !isUnread(item) && (nested ? 'text-muted' : 'text-ink/85')]">{{ item.title }}</span><span class="ml-auto shrink-0 text-[10px] tabular-nums text-subtle phone:visible! group-hover:invisible group-has-[[aria-expanded=true]]:invisible">{{ shortAge(item.at, now) }}</span></span>
              <span v-if="!nested || !item.finished" class="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11px] text-subtle">
                <span :class="item.finished ? 'sr-only' : ['truncate font-medium', tone(item)]">{{ label(item) }}</span><span v-if="!item.finished && !nested">·</span><template v-if="!nested"><HarnessMark v-if="item.harness" :provider="item.harness" /><span class="truncate">{{ item.project || item.agent }}</span></template>
              </span>
              <span v-if="item.requester && !nested" class="mt-0.5 flex items-center gap-1 truncate text-[10px] text-subtle" data-testid="fil-requester"><Icon :name="CornerDownRight" :size="10" />Delegated by {{ item.requester.title }}</span>
              <span v-if="item.originRule" class="mt-0.5 block truncate text-[10px] text-subtle">{{ item.originRule.schedule ? 'Schedule' : 'Rule' }} · {{ item.originRule.ruleName }}</span>
              <span v-if="item.wakeAt !== undefined" class="mt-0.5 block text-[10px] text-accent">{{ waitLeft({ wakeAt: item.wakeAt, condition: item.condition, resolved: item.resolved }, now) }}</span>
            </span>
          </button>
          <button
            v-if="isUnread(item) && item.readAt"
            type="button"
            :aria-label="`Mark “${item.title}” as read`"
            title="Mark as read"
            class="absolute right-9 top-1.5 grid size-7 shrink-0 place-items-center rounded-md bg-hover text-muted opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-ink phone:static phone:bg-transparent phone:opacity-100"
            @click="read(item)"
          >
            <Icon :name="Check" :size="15" />
          </button>
          <MenuButton
            :label="`More actions for “${item.title}”`"
            trigger-class="absolute right-1 top-1.5 grid size-7 shrink-0 place-items-center rounded-md bg-hover text-muted opacity-0 group-hover:opacity-100 focus:opacity-100 hover:text-ink aria-expanded:opacity-100 phone:static phone:mr-1 phone:bg-transparent phone:opacity-100"
            :width="210"
          >
            <template #trigger>
              <Icon :name="MoreHorizontal" :size="15" />
            </template>
            <template #default="{ select }">
              <button
                v-if="(isUnread(item) || item.kind === 'snoozed') && item.readAt"
                type="button"
                role="menuitem"
                tabindex="-1"
                class="menu-item"
                @click="select(() => read(item))"
              >
                <Icon :name="Check" :size="16" />Mark as read
              </button>
              <button
                v-if="item.kind === 'snoozed'"
                type="button"
                role="menuitem"
                tabindex="-1"
                class="menu-item"
                @click="select(() => restoreConversation(item.chatId))"
              >
                <Icon :name="Clock" :size="16" />Restore
              </button>
              <button
                v-else
                type="button"
                role="menuitem"
                tabindex="-1"
                class="menu-item"
                @click="select(() => openSnooze(item))"
              >
                <Icon :name="Clock" :size="16" />Snooze until…
              </button>
            </template>
          </MenuButton>
        </article>
      </template>
    </div>
    <Modal v-if="snoozing" title="Snooze conversation" @close="snoozing = null">
      <form class="grid gap-4 p-5" @submit.prevent="snooze">
        <label>Until<input v-model="snoozeDate" type="datetime-local" required></label>
        <UiButton type="submit" variant="primary" :disabled="!(snoozeDeadline > now)">
          Snooze
        </UiButton>
      </form>
    </Modal>
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
          ref="peekCard"
          role="tooltip"
          class="fixed z-40 w-80 rounded-xl border border-line bg-raised p-3.5 shadow-[0_12px_32px_#0000002e]"
          :style="{ top: `${peeked.top}px`, left: `${peeked.left}px` }"
          @mouseenter="keepPeek"
          @mouseleave="leavePeek"
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
