<script setup lang="ts">
import type { ChatView } from '../../shared/chats'
import type { FilItem } from '../signal'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { api, notify, state } from '../api'
import {
  CheckCheck,
  ListFilter,
  RotateCcw,
  Search,
  X,
} from '../icons'
import { refreshMissionRuns, useMissionRuns } from '../mission-runs'
import { typingTarget } from '../shortcuts'
import { filOf, greeting, shortAge } from '../signal'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'

// The Fil: what needs the user, live work and recent conversations. J/K move the selection,
// Enter opens it and / filters, as in a mail client.
const props = withDefaults(defineProps<{ chats: ChatView[], selected?: string | null, compact?: boolean }>(), { selected: null, compact: false })
const router = useRouter()
const runs = useMissionRuns()
const filter = ref('')
const filterInput = ref<HTMLInputElement>()
const cursor = ref<string | null>(null)
const now = ref(Date.now())
const clock = setInterval(() => now.value = Date.now(), 30000)
onBeforeUnmount(() => clearInterval(clock))

const fil = computed(() => filOf(props.chats, state.tasks, runs.value))

function matches(item: FilItem) {
  const query = filter.value.trim().toLowerCase()
  return !query || [item.title, item.subtitle, item.agent, item.project ?? ''].some(value => value.toLowerCase().includes(query))
}

const sections = computed(() => [
  { key: 'for-you', label: 'For you', items: fil.value.forYou.filter(matches) },
  { key: 'live', label: 'Live', items: fil.value.live.filter(matches) },
  { key: 'recent', label: 'Recent', items: fil.value.recent.filter(matches) },
])
const flat = computed(() => sections.value.flatMap(section => section.items))
const summary = computed(() => {
  const need = fil.value.forYou.length
  const working = fil.value.live.filter(item => item.kind === 'running').length
  return [need ? `${need} need${need === 1 ? 's' : ''} you` : 'Nothing needs you', working ? `${working} working` : ''].filter(Boolean).join(' · ')
})
watch(() => props.selected, (id) => {
  if (id)
    cursor.value = `chat:${id}`
})

function current(item: FilItem) {
  return (item.chatId && item.chatId === props.selected) || cursor.value === item.key
}

function badge(item: FilItem) {
  return item.kind === 'question' ? 'question' : item.kind === 'failed' || item.kind === 'failed-mission' ? 'failed' : item.kind === 'queued' ? 'queued' : item.kind === 'paused' ? 'paused' : null
}

const labels: Partial<Record<FilItem['kind'], string>> = {
  'question': 'Needs your answer',
  'failed': 'Failed',
  'failed-mission': 'Mission failed',
  'review': 'Needs review',
}

function open(item: FilItem) {
  cursor.value = item.key
  void router.push(item.to)
}

async function runAgain(item: FilItem) {
  try {
    await api(`/tasks/${item.taskId}/run`, { method: 'POST' })
    notify(`${item.title} started`)
    await refreshMissionRuns()
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
    <header class="shrink-0 px-5 pb-3" :class="compact ? 'pt-5' : 'pt-6 phone:pt-[max(18px,env(safe-area-inset-top))]'">
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
            <span class="eyebrow" :class="{ 'text-coral!': section.key === 'for-you' && section.items.length }">{{ section.label }}</span>
            <span class="rounded-full px-1.5 text-[10.5px] font-bold tabular-nums" :class="section.key === 'for-you' && section.items.length ? 'bg-coral-soft text-coral' : 'bg-hover text-muted'">{{ section.items.length }}</span>
            <span v-if="section.key === 'live' && section.items.some(item => item.kind === 'running')" class="live-dot ml-auto mr-1" />
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
                :class="current(item) ? 'border-accent shadow-focus' : 'border-line'"
              >
                <span class="absolute inset-y-0 left-0 w-[3px]" :class="item.kind === 'review' ? 'bg-warning' : 'bg-coral'" />
                <button type="button" class="flex w-full items-start gap-3 px-4 pb-3 pt-3.5 text-left" @click="open(item)">
                  <AgentAvatar
                    :name="item.agent"
                    :identity="item.agentKey"
                    :size="34"
                    :badge="badge(item)"
                  />
                  <span class="min-w-0 flex-1">
                    <span class="flex items-center gap-2 whitespace-nowrap text-[11.5px] font-semibold">
                      <span :class="item.kind === 'review' ? 'text-warning' : 'text-coral'">{{ labels[item.kind] }}</span>
                      <span class="text-subtle">·</span>
                      <span class="truncate font-medium text-muted">{{ [item.agent, item.project].filter(Boolean).join(' · ') }}</span>
                      <span class="ml-auto shrink-0 font-medium text-subtle">{{ shortAge(item.at, now) }}</span>
                    </span>
                    <span class="mt-1 line-clamp-2 block font-heading text-[14.5px] font-bold leading-snug text-ink">{{ item.title }}</span>
                    <span class="mt-1 line-clamp-1 text-[12.5px] leading-relaxed text-muted phone:line-clamp-2">{{ item.subtitle }}</span>
                  </span>
                </button>
                <div class="flex gap-1.5 px-4 pb-3.5 pl-[62px]">
                  <button
                    v-if="item.kind === 'failed-mission'"
                    type="button"
                    :aria-label="`Run “${item.title}” again`"
                    class="press flex h-8 items-center gap-1.5 rounded-full bg-coral-soft px-3 text-[12.5px] font-semibold text-coral"
                    @click="runAgain(item)"
                  >
                    <Icon :name="RotateCcw" :size="14" /> Run again
                  </button>
                  <button
                    type="button"
                    :aria-label="`${item.kind === 'question' ? 'Answer' : item.kind === 'failed' ? 'Resume' : 'Open'} “${item.title}”`"
                    class="press h-8 rounded-full px-3 text-[12.5px] font-semibold"
                    :class="item.kind === 'question' ? 'bg-accent text-surface' : 'text-muted hover:bg-hover'"
                    @click="open(item)"
                  >
                    {{ item.kind === 'question' ? 'Answer' : item.kind === 'failed' ? 'Resume' : 'Open' }}
                  </button>
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
                :aria-current="item.chatId && item.chatId === selected ? 'page' : undefined"
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
                  <span class="block truncate text-[13.5px] font-semibold leading-tight text-ink">{{ item.title }}</span>
                  <span class="mt-1 flex min-w-0 items-center text-[12px] leading-tight">
                    <template v-if="item.kind === 'running'">
                      <span class="shrink-0 font-semibold text-accent">{{ item.taskId ? 'Mission running' : 'Working' }}</span>
                      <span class="working-wave text-accent"><i /><i /><i /></span>
                      <span v-if="item.project" class="ml-2 truncate text-muted">{{ item.project }}</span>
                    </template>
                    <template v-else-if="item.kind === 'queued'">
                      <span class="shrink-0 font-semibold text-muted">Queued</span>
                      <span class="ml-1.5 truncate text-subtle">· {{ item.subtitle }}</span>
                    </template>
                    <template v-else>
                      <span class="shrink-0 font-medium text-muted">{{ item.agent }}</span>
                      <span class="ml-1.5 truncate text-subtle">· {{ item.kind === 'paused' ? 'Paused' : item.project || 'No project' }}</span>
                    </template>
                  </span>
                </span>
                <span class="shrink-0 self-start pt-0.5 text-[11px] tabular-nums" :class="item.kind === 'running' ? 'font-semibold text-accent' : 'text-subtle'">{{ shortAge(item.at, now) }}</span>
              </button>
            </template>
          </TransitionGroup>
        </section>
      </template>
      <p v-if="!compact" class="mt-6 flex flex-wrap items-center justify-center gap-x-3 gap-y-1 text-[11.5px] text-subtle phone:hidden">
        <span><kbd class="keycap">J</kbd> <kbd class="keycap">K</kbd> move</span>
        <span><kbd class="keycap">C</kbd> new</span>
        <span class="inline-flex items-center gap-1"><Icon :name="Search" :size="12" /><kbd class="keycap">⌘ K</kbd></span>
        <span><kbd class="keycap">?</kbd> shortcuts</span>
      </p>
    </div>
  </div>
</template>
