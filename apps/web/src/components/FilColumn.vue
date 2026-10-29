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
import { canUseAgent } from '../agent-use'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import { waitLeft } from '../chat-wait'
import { setAside } from '../dismissals'
import {
  Archive,
  Check,
  CheckCheck,
  Clock,
  CornerDownRight,
  ListFilter,
  MoreHorizontal,
  RotateCcw,
  Search,
  X,
} from '../icons'
import { refreshMissionRuns, useMissionRuns } from '../mission-runs'
import { localReads, markRead } from '../reads'
import { typingTarget } from '../shortcuts'
import {
  dismissible,
  filOf,
  greeting,
  isUnread,
  outcomeLabels,
  shortAge,
  tomorrowMorning,
} from '../signal'
import AgentAvatar from './AgentAvatar.vue'
import HarnessMark from './HarnessMark.vue'
import Icon from './Icon.vue'
import InstallationHeader from './InstallationHeader.vue'
import MenuButton from './MenuButton.vue'

// The Fil: what needs the user, live work, waiting and recent conversations. J/K move the selection,
// Enter opens it, E marks finished work as read or dismisses a blocked mission, and / filters, as in
// a mail client.
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
const now = ref(Date.now())
const clock = setInterval(() => now.value = Date.now(), 30000)
onBeforeUnmount(() => clearInterval(clock))

const fil = computed(() => filOf(props.chats, state.tasks, runs.value, localReads, now.value))
const opened = (item: FilItem) => (item.chatId && item.chatId === props.selected) || (item.runId && item.runId === props.selectedRun)

// Finished work opened from For you stays there, read, while it is open: it moves to Recent once
// the user moves on rather than jumping away under the cursor. Work that finishes while it is
// open was read as it happened and goes straight to Recent.
const lingering = ref<FilItem | null>(null)
watch(() => [props.selected, props.selectedRun], () => {
  lingering.value = fil.value.forYou.find(item => isUnread(item) && opened(item)) ?? null
}, { immediate: true })
const readingNow = (item: FilItem) => isUnread(item) && opened(item) && item.key !== lingering.value?.key
const forYou = computed(() => {
  const items = fil.value.forYou.filter(item => !readingNow(item))
  const kept = lingering.value
  if (!kept || [...items, ...fil.value.live, ...fil.value.waiting].some(item => item.key === kept.key))
    return items
  const firstUnread = items.findIndex(isUnread)
  const at = firstUnread < 0 ? items.length : firstUnread
  return [...items.slice(0, at), kept, ...items.slice(at)]
})
// A mission read as it finishes simply leaves For you; only blocked ones set aside reach Recent.
const recent = computed(() => [
  ...fil.value.forYou.filter(item => item.chatId && readingNow(item)).map(item => ({ ...item, kind: 'recent' as const, preview: undefined })),
  ...fil.value.recent.filter(item => item.key !== lingering.value?.key),
].sort((a, b) => b.at - a.at))
const unread = computed(() => fil.value.forYou.filter(item => isUnread(item) && !readingNow(item)))
const read = (item: FilItem) => isUnread(item) && !fil.value.forYou.some(other => other.key === item.key)

function matches(item: FilItem) {
  const query = filter.value.trim().toLowerCase()
  return !query || [item.title, item.subtitle, item.agent, item.project ?? ''].some(value => value.toLowerCase().includes(query))
}

const sections = computed(() => [
  { key: 'for-you', label: 'For you', items: forYou.value.filter(matches) },
  { key: 'live', label: 'Live', items: fil.value.live.filter(matches) },
  { key: 'waiting', label: 'Waiting', items: fil.value.waiting.filter(matches) },
  { key: 'recent', label: 'Recent', items: recent.value.filter(matches) },
])
const flat = computed(() => sections.value.flatMap(section => section.items))
const summary = computed(() => {
  const need = fil.value.forYou.filter(item => !isUnread(item)).length
  const working = fil.value.live.filter(item => item.kind === 'running').length
  const suspended = fil.value.live.filter(item => item.kind === 'suspended').length
  const waiting = fil.value.waiting.length
  return [
    need ? `${need} need${need === 1 ? 's' : ''} you` : unread.value.length ? '' : 'Nothing needs you',
    unread.value.length ? `${unread.value.length} to read` : '',
    working ? `${working} working` : '',
    suspended ? `${suspended} suspended` : '',
    waiting ? `${waiting} waiting` : '',
  ].filter(Boolean).join(' · ')
})
watch(() => props.selected, (id) => {
  if (id)
    cursor.value = `chat:${id}`
})
watch(() => props.selectedRun, (id) => {
  const item = id && flat.value.find(item => item.runId === id)
  if (item)
    cursor.value = item.key
}, { immediate: true })

function current(item: FilItem) {
  return opened(item) || cursor.value === item.key
}

function badge(item: FilItem) {
  return item.kind === 'question' ? 'question' : item.kind === 'failed' || item.kind === 'failed-mission' ? 'failed' : item.kind === 'unread' ? 'done' : item.kind === 'queued' ? 'queued' : item.kind === 'waiting' || item.kind === 'snoozed' ? 'waiting' : item.kind === 'paused' || item.kind === 'suspended' ? 'paused' : null
}

const labels: Partial<Record<FilItem['kind'], string>> = {
  'question': 'Needs your answer',
  'failed': 'Failed',
  'failed-mission': 'Mission failed',
  'review': 'Needs review',
}

function label(item: FilItem) {
  if (isUnread(item))
    return item.taskId ? 'Mission finished' : 'Finished'
  return labels[item.kind]
}

// The coloured edge says what kind of attention a card asks for.
function edge(item: FilItem) {
  if (read(item))
    return 'bg-line'
  return item.kind === 'review' ? 'bg-warning' : isUnread(item) ? 'bg-accent' : 'bg-coral'
}

function tone(item: FilItem) {
  if (read(item))
    return 'text-muted'
  return item.kind === 'review' ? 'text-warning' : isUnread(item) ? 'text-accent' : 'text-coral'
}

function action(item: FilItem) {
  const usable = canUseAgent(state.usableAgents, item.agentKey)
  return item.kind === 'question' ? (usable ? 'Answer' : 'Open') : item.kind === 'failed' ? (usable ? 'Resume' : 'Open') : isUnread(item) && !read(item) ? 'Read' : 'Open'
}

function open(item: FilItem) {
  cursor.value = item.key
  void router.push(item.to)
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

// A blocked mission's outcome, for the account's dismissal or snooze.
const outcomeOf = (item: FilItem) => ({ id: item.runId!, taskId: item.taskId!, finishedAt: item.at })

function dismiss(item: FilItem) {
  void setAside(outcomeOf(item))
}

function snooze(item: FilItem) {
  void setAside(outcomeOf(item), tomorrowMorning(now.value))
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
  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || typingTarget(event.target) || document.querySelector('dialog[open]'))
    return
  if (event.key === 'j' || event.key === 'k') {
    event.preventDefault()
    move(event.key === 'j' ? 1 : -1)
  }
  // Enter belongs to a focused control; it opens the selection only when focus rests on the page.
  else if (event.key === 'Enter' && cursor.value && !(event.target instanceof HTMLElement && event.target.closest('a, button, input, textarea, select, summary, [tabindex]'))) {
    const item = flat.value.find(item => item.key === cursor.value)
    if (item) {
      event.preventDefault()
      open(item)
    }
  }
  else if (event.key === 'e') {
    const item = flat.value.find(item => item.key === cursor.value)
    if (item && isUnread(item) && !read(item)) {
      event.preventDefault()
      void markRead([item])
    }
    else if (item && dismissible(item)) {
      event.preventDefault()
      dismiss(item)
    }
  }
  else if (event.key === '/') {
    event.preventDefault()
    filterInput.value?.focus()
  }
}

onMounted(() => document.addEventListener('keydown', key))
onBeforeUnmount(() => document.removeEventListener('keydown', key))
</script>

<template>
  <div class="fil flex h-full min-h-0 flex-col">
    <header class="shrink-0 px-5 pb-3" :class="compact ? 'pt-5' : 'pt-6 phone:px-4 phone:pt-[max(10px,env(safe-area-inset-top))]'">
      <InstallationHeader v-if="!compact" class="mb-4" />
      <div class="flex items-start gap-3">
        <div class="min-w-0 flex-1">
          <p class="eyebrow">
            {{ greeting() }}
          </p>
          <h1 class="mt-1 font-heading text-[28px]! font-extrabold leading-none! tracking-[-0.03em]! text-ink phone:text-[34px]!">
            Fil
          </h1>
          <p class="mt-2 text-[13px] text-muted">
            {{ summary }}
          </p>
        </div>
      </div>
      <label class="fil-filter mt-4 flex h-10 flex-row! items-center gap-2 rounded-xl border border-line bg-surface px-3 text-muted transition focus-within:border-accent focus-within:shadow-focus">
        <Icon :name="ListFilter" :size="16" />
        <input
          ref="filterInput"
          v-model="filter"
          class="min-h-0! min-w-0 flex-1 border-0! bg-transparent! p-0! text-[13px] text-ink shadow-none! outline-none placeholder:text-subtle"
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
        >
          <Icon :name="X" :size="14" />
        </button>
      </label>
    </header>

    <div class="min-h-0 flex-1 overflow-y-auto overscroll-contain px-3 pb-10 [scrollbar-width:thin] phone:pb-32">
      <p v-if="filter && !flat.length" class="px-3 py-10 text-center text-[13px] text-muted">
        Nothing matches “{{ filter }}”.
      </p>
      <template v-for="section in sections" :key="section.key">
        <section v-if="section.items.length || section.key === 'for-you'" class="mt-3" :aria-label="section.label">
          <h2 class="flex items-center gap-2 px-3 pb-2 pt-2 text-base!">
            <span class="eyebrow" :class="{ 'text-coral!': section.key === 'for-you' && section.items.some(item => !isUnread(item)), 'text-accent!': section.key === 'for-you' && section.items.length && section.items.every(isUnread) }">{{ section.label }}</span>
            <span class="rounded-full px-1.5 text-[10.5px] font-bold tabular-nums" :class="section.key !== 'for-you' || !section.items.length ? 'bg-hover text-muted' : section.items.some(item => !isUnread(item)) ? 'bg-coral-soft text-coral' : 'bg-soft text-accent'">{{ section.items.length }}</span>
            <span v-if="section.key === 'live' && section.items.some(item => item.kind === 'running')" class="live-dot ml-auto mr-1" />
            <button
              v-if="section.key === 'for-you' && unread.length"
              type="button"
              class="press -my-1 ml-auto flex h-7 items-center gap-1.5 rounded-full px-2.5 text-[11.5px] font-semibold text-muted transition-colors hover:bg-hover hover:text-ink"
              :aria-label="`Mark ${unread.length} finished ${unread.length === 1 ? 'item' : 'items'} as read`"
              @click="markRead(unread)"
            >
              <Icon :name="CheckCheck" :size="14" /> Mark all read
            </button>
          </h2>
          <p v-if="section.key === 'for-you' && !section.items.length && !filter" class="mx-2 flex items-center gap-2 rounded-2xl border border-dashed border-line px-4 py-3 text-[13px] text-muted">
            <Icon :name="CheckCheck" :size="16" class="text-success" /> All clear — nothing is waiting on you.
          </p>
          <TransitionGroup
            tag="div"
            name="fil-row"
            class="relative flex flex-col"
            :class="section.key === 'for-you' ? 'gap-2 px-1' : 'gap-0.5'"
          >
            <template v-if="section.key === 'for-you'">
              <article
                v-for="item in section.items"
                :key="item.key"
                :data-fil-key="item.key"
                class="lift relative overflow-hidden rounded-2xl border bg-surface"
                :class="[current(item) ? 'border-accent shadow-focus' : 'border-line', { 'fil-unread': isUnread(item) && !read(item) }]"
                :data-unread="isUnread(item) && !read(item) ? '' : undefined"
              >
                <span class="absolute inset-y-0 left-0 w-[3px] transition-colors duration-500" :class="edge(item)" />
                <button type="button" class="flex w-full items-start gap-3 px-4 pb-3 pt-3.5 text-left" @click="open(item)">
                  <AgentAvatar
                    :name="item.agent"
                    :identity="item.agentKey"
                    :size="34"
                    :badge="badge(item)"
                  />
                  <span class="min-w-0 flex-1">
                    <span v-if="item.chatId && item.requester" class="mb-1 flex min-w-0 items-center gap-1 text-[11px] leading-tight text-subtle" data-testid="fil-requester">
                      <Icon :name="CornerDownRight" :size="11" class="shrink-0" /><span class="truncate">Delegated by {{ item.requester.title }}</span>
                    </span>
                    <span class="flex items-center gap-2 whitespace-nowrap text-[11.5px] font-semibold">
                      <span
                        v-if="isUnread(item)"
                        class="unread-dot"
                        :class="{ 'unread-dot-read': read(item) }"
                        :aria-label="read(item) ? undefined : 'Unread'"
                        :role="read(item) ? undefined : 'img'"
                      />
                      <span class="transition-colors duration-500" :class="tone(item)">{{ read(item) ? 'Read' : label(item) }}</span>
                      <span class="text-subtle">·</span>
                      <span class="flex min-w-0 items-center gap-1 font-medium text-muted"><span class="truncate">{{ item.agent }}</span><template v-if="item.harness || item.project"> · <HarnessMark v-if="item.harness" :provider="item.harness" /> <span v-if="item.project" class="truncate">{{ item.project }}</span></template></span>
                      <span class="ml-auto shrink-0 font-medium text-subtle">{{ shortAge(item.at, now) }}</span>
                    </span>
                    <span class="mt-1 line-clamp-2 block font-heading text-[14.5px] font-bold leading-snug text-ink">{{ item.title }}</span>
                    <span v-if="item.preview" class="fil-preview mt-2 line-clamp-2 block rounded-lg bg-inset px-2.5 py-1.5 text-[12.5px] leading-relaxed text-muted phone:line-clamp-3">{{ item.preview }}</span>
                    <span v-else class="mt-1 line-clamp-1 text-[12.5px] leading-relaxed text-muted phone:line-clamp-2">{{ item.subtitle }}</span>
                  </span>
                </button>
                <div class="flex gap-1.5 px-4 pb-3.5 pl-[62px]">
                  <button
                    v-if="item.kind === 'failed-mission' && canUseAgent(state.usableAgents, item.agentKey)"
                    type="button"
                    :aria-label="`Run “${item.title}” again`"
                    class="press flex h-8 items-center gap-1.5 rounded-full bg-coral-soft px-3 text-[12.5px] font-semibold text-coral"
                    @click="runAgain(item)"
                  >
                    <Icon :name="RotateCcw" :size="14" /> Run again
                  </button>
                  <button
                    type="button"
                    :aria-label="`${action(item)} “${item.title}”`"
                    class="press h-8 rounded-full px-3 text-[12.5px] font-semibold"
                    :class="item.kind === 'question' ? 'bg-accent text-surface' : isUnread(item) && !read(item) ? 'bg-soft text-accent' : 'text-muted hover:bg-hover'"
                    @click="open(item)"
                  >
                    {{ action(item) }}
                  </button>
                  <button
                    v-if="isUnread(item) && !read(item)"
                    type="button"
                    :aria-label="`Mark “${item.title}” as read`"
                    title="Mark as read (E)"
                    class="press flex h-8 items-center gap-1.5 rounded-full px-3 text-[12.5px] font-semibold text-muted hover:bg-hover hover:text-ink"
                    @click="markRead([item])"
                  >
                    <Icon :name="Check" :size="14" /> Mark as read
                  </button>
                  <button
                    v-if="dismissible(item)"
                    type="button"
                    :aria-label="`Dismiss “${item.title}”`"
                    title="Nothing to do on your side: it leaves For you until the mission reports again (E)"
                    class="press flex h-8 items-center gap-1.5 rounded-full px-3 text-[12.5px] font-semibold text-muted hover:bg-hover hover:text-ink"
                    @click="dismiss(item)"
                  >
                    <Icon :name="Check" :size="14" /> Dismiss
                  </button>
                  <MenuButton
                    v-if="item.kind === 'review'"
                    :label="`More actions for “${item.title}”`"
                    title="More actions"
                    trigger-class="press ml-auto grid size-8 place-items-center rounded-full text-muted hover:bg-hover hover:text-ink"
                    :width="232"
                  >
                    <template #trigger>
                      <Icon :name="MoreHorizontal" :size="16" />
                    </template>
                    <template #default="{ select }">
                      <button
                        type="button"
                        role="menuitem"
                        tabindex="-1"
                        class="menu-item"
                        @click="select(() => snooze(item))"
                      >
                        <Icon :name="Clock" :size="16" />Remind me tomorrow
                      </button>
                      <button
                        v-if="canUseAgent(state.usableAgents, item.agentKey)"
                        type="button"
                        role="menuitem"
                        tabindex="-1"
                        class="menu-item"
                        @click="select(() => runAgain(item))"
                      >
                        <Icon :name="RotateCcw" :size="16" />Run again
                      </button>
                      <button
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
                </div>
              </article>
            </template>
            <template v-else>
              <button
                v-for="item in section.items"
                :key="item.key"
                type="button"
                :data-fil-key="item.key"
                class="group relative flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left transition-colors duration-150"
                :class="current(item) ? 'bg-hover' : 'hover:bg-hover/70'"
                :aria-current="opened(item) ? 'page' : undefined"
                @click="open(item)"
              >
                <span class="absolute left-0 top-1/2 h-6 w-[3px] -translate-y-1/2 rounded-full bg-accent transition-transform duration-300" :class="current(item) ? 'scale-y-100' : 'scale-y-0'" />
                <AgentAvatar
                  :name="item.agent"
                  :identity="item.agentKey"
                  :size="34"
                  :working="item.kind === 'running'"
                  :badge="badge(item)"
                />
                <span class="min-w-0 flex-1">
                  <span v-if="item.chatId && item.requester" class="mb-0.5 flex min-w-0 items-center gap-1 text-[11px] leading-tight text-subtle" data-testid="fil-requester">
                    <Icon :name="CornerDownRight" :size="11" class="shrink-0" /><span class="truncate">Delegated by {{ item.requester.title }}</span>
                  </span>
                  <span class="block truncate text-[13.5px] font-semibold leading-tight text-ink">{{ item.title }}</span>
                  <span class="mt-1 flex min-w-0 items-center text-[12px] leading-tight">
                    <template v-if="item.kind === 'running'">
                      <span class="shrink-0 font-semibold text-accent">{{ item.taskId ? 'Mission running' : 'Working' }}</span>
                      <span class="working-wave text-accent"><i /><i /><i /></span>
                      <span v-if="item.harness || item.project" class="ml-2 flex min-w-0 items-center gap-1 text-muted"><HarnessMark v-if="item.harness" :provider="item.harness" /> <span v-if="item.project" class="truncate">{{ item.project }}</span></span>
                    </template>
                    <template v-else-if="item.kind === 'queued'">
                      <span class="shrink-0 font-semibold text-muted">Queued</span>
                      <span class="ml-1.5 flex min-w-0 items-center gap-1 text-subtle">· <HarnessMark v-if="item.harness" :provider="item.harness" /> <span class="truncate">{{ item.subtitle }}</span></span>
                    </template>
                    <template v-else-if="item.kind === 'waiting'">
                      <span class="truncate font-medium text-accent">{{ item.subtitle }}</span>
                      <span v-if="item.harness || item.project" class="ml-1.5 flex min-w-0 items-center gap-1 text-subtle">· <HarnessMark v-if="item.harness" :provider="item.harness" /> <span v-if="item.project" class="truncate">{{ item.project }}</span></span>
                    </template>
                    <template v-else-if="item.kind === 'snoozed' || item.kind === 'set-aside'">
                      <span class="shrink-0 font-medium" :class="item.kind === 'snoozed' ? 'text-accent' : 'text-warning'">{{ item.kind === 'snoozed' ? 'Snoozed' : outcomeLabels[item.outcome!] }}</span>
                      <span class="ml-1.5 flex min-w-0 items-center gap-1 text-subtle">· <HarnessMark v-if="item.harness" :provider="item.harness" /> <span class="truncate">{{ item.kind === 'snoozed' ? outcomeLabels[item.outcome!] : item.subtitle }}</span></span>
                    </template>
                    <template v-else-if="item.kind === 'suspended'">
                      <span class="flex min-w-0 items-center gap-1 font-medium text-muted"><HarnessMark v-if="item.harness" :provider="item.harness" /> <span class="truncate">{{ item.subtitle }}</span></span>
                    </template>
                    <template v-else>
                      <span class="shrink-0 font-medium text-muted">{{ item.agent }}</span>
                      <span class="ml-1.5 flex min-w-0 items-center gap-1 text-subtle">· <HarnessMark v-if="item.harness" :provider="item.harness" /> <span class="truncate">{{ item.kind === 'paused' ? 'Paused' : item.project || 'No project' }}</span></span>
                    </template>
                  </span>
                </span>
                <span
                  v-if="(item.kind === 'waiting' || item.kind === 'snoozed') && item.wakeAt !== undefined"
                  class="shrink-0 self-start pt-0.5 text-[11px] font-semibold tabular-nums text-accent"
                  :title="item.kind === 'snoozed' ? 'Time left before it is back in For you' : item.condition ? 'Resumes when the result arrives, at the latest by its deadline' : 'Time left before it resumes'"
                >{{ waitLeft({ wakeAt: item.wakeAt!, condition: item.condition, resolved: item.resolved }, now) }}</span>
                <span v-else-if="item.kind === 'waiting'" class="shrink-0 self-start pt-0.5 text-[11px] text-accent">Awaiting availability</span>
                <span v-else class="shrink-0 self-start pt-0.5 text-[11px] tabular-nums" :class="item.kind === 'running' ? 'font-semibold text-accent' : 'text-subtle'">{{ shortAge(item.at, now) }}</span>
              </button>
            </template>
          </TransitionGroup>
        </section>
      </template>
      <p v-if="!compact" class="mt-6 flex flex-wrap items-center justify-center gap-x-3 gap-y-1 text-[11.5px] text-subtle phone:hidden">
        <span><kbd class="keycap">J</kbd> <kbd class="keycap">K</kbd> move</span>
        <span v-if="unread.length"><kbd class="keycap">E</kbd> read</span>
        <span v-else-if="forYou.some(dismissible)"><kbd class="keycap">E</kbd> dismiss</span>
        <span><kbd class="keycap">C</kbd> new</span>
        <span class="inline-flex items-center gap-1"><Icon :name="Search" :size="12" /><kbd class="keycap">⌘ K</kbd></span>
        <span><kbd class="keycap">?</kbd> shortcuts</span>
      </p>
    </div>
  </div>
</template>
