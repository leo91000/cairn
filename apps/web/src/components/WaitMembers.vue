<script setup lang="ts">
import type { WaitMember, WaitTone } from '../chat-wait'
import type { IconName } from '../icons'
import { computed, ref, useId } from 'vue'
import { timeLeft } from '../chat-wait'
import {
  ArrowUpRight,
  Check,
  ChevronDown,
  Minus,
  TriangleAlert,
  X,
} from '../icons'
import Icon from './Icon.vue'

// The conditions of a grouped wait, each with its own result as it arrives. A segmented meter
// sums them up; the list stays open until the conversation moves past the wait.
const props = defineProps<{
  members: WaitMember[]
  mode: 'all' | 'any'
  /** The wait still counts down: pending conditions animate and plugin checks are timed. */
  live: boolean
  /** The wait ended, by its results or its deadline. */
  finished: boolean
  timedOut: boolean
  /** Folded by default: a wait the conversation has moved past. */
  folded?: boolean
  now: number
}>()

const listId = useId()
const toggled = ref<boolean | null>(null)
const open = computed(() => toggled.value ?? !props.folded)
const received = computed(() => props.members.filter(member => member.outcome).length)
// With `any`, the first result in the agent's order is the one that resumed the conversation.
const first = computed(() => props.mode === 'any' && props.finished ? props.members.findIndex(member => member.outcome) : -1)

const pageSize = 10
const selectedPage = ref(0)
const pageCount = computed(() => Math.ceil(props.members.length / pageSize))
const page = computed(() => Math.min(selectedPage.value, Math.max(0, pageCount.value - 1)))
const start = computed(() => page.value * pageSize)
const visible = computed(() => props.members.slice(start.value, start.value + pageSize))

type State = WaitTone | 'pending' | 'idle'

function state(member: WaitMember): State {
  if (member.outcome)
    return member.outcome.tone
  return props.live ? 'pending' : 'idle'
}

const glyphs: Record<State, { icon?: IconName, class: string }> = {
  ok: { icon: Check, class: 'bg-success text-surface' },
  error: { icon: X, class: 'bg-danger text-surface' },
  warning: { icon: TriangleAlert, class: 'bg-warning-surface text-warning' },
  neutral: { icon: Check, class: 'bg-variant text-muted' },
  pending: { class: 'wait-member-spinner' },
  idle: { icon: Minus, class: 'border border-dashed border-line text-subtle' },
}
const segments: Record<State, string> = {
  ok: 'bg-success',
  error: 'bg-danger',
  warning: 'bg-warning',
  neutral: 'bg-subtle',
  pending: 'wait-member-shimmer',
  idle: 'bg-variant',
}

// Small groups keep their individual segments; large groups share one segment per state.
const meter = computed(() => {
  if (props.members.length <= pageSize)
    return props.members.map((member, index) => ({ state: state(member), count: 1, first: index === first.value }))
  const counts = new Map<State, number>()
  for (const member of props.members) {
    const key = state(member)
    counts.set(key, (counts.get(key) ?? 0) + 1)
  }

  return [...counts].map(([state, count]) => ({ state, count, first: false }))
})

/** What a condition shows beside its name: its result, or how Cairn follows it. */
function status(member: WaitMember) {
  if (member.outcome)
    return [member.outcome.title, member.outcome.detail].filter(Boolean).join(' · ')
  if (props.finished)
    return props.timedOut ? 'No result before the deadline' : 'Not needed: another result came first'
  if (!props.live)
    return 'Awaiting its result'
  if (member.lastError)
    return 'Check failed · retrying'
  if (member.kind === 'run' || member.kind === 'task')
    return 'Waiting for it to finish'
  if (member.nextCheckAt)
    return member.nextCheckAt > props.now ? `Next check in ${timeLeft(member.nextCheckAt - props.now)}` : 'Checking…'
  return 'Awaiting its result'
}

const tones: Record<State, string> = {
  ok: 'text-success',
  error: 'text-danger',
  warning: 'text-warning',
  neutral: 'text-muted',
  pending: 'text-muted',
  idle: 'text-subtle',
}
</script>

<template>
  <div class="wait-members" data-testid="wait-members">
    <button
      type="button"
      class="flex w-full items-center gap-2.5 rounded-lg py-1 text-left"
      :aria-expanded="open"
      :aria-controls="listId"
      @click="toggled = !open"
    >
      <span
        class="shrink-0 rounded-full px-2 py-0.5 text-2xs font-bold uppercase tracking-wide"
        :class="mode === 'any' ? 'bg-coral-soft text-coral' : 'bg-soft text-accent'"
        :title="mode === 'any' ? 'Resumes at the first result' : 'Resumes once every condition has a result'"
      >{{ mode === 'any' ? 'Any' : 'All' }}</span>
      <span class="flex min-w-0 flex-1 gap-1" aria-hidden="true">
        <span
          v-for="(segment, index) in meter"
          :key="index"
          class="h-1.5 min-w-0 rounded-full transition-colors duration-500"
          :style="{ flexGrow: segment.count, flexBasis: 0 }"
          :class="[segments[segment.state], segment.first ? 'ring-2 ring-coral/30' : '']"
        />
      </span>
      <span class="shrink-0 text-2xs font-semibold tabular-nums text-muted">
        {{ received }}/{{ members.length }}<span class="sr-only"> results</span>
      </span>
      <Icon
        :name="ChevronDown"
        :size="15"
        class="shrink-0 text-subtle transition-transform"
        :class="open ? 'rotate-180' : ''"
      />
    </button>
    <ol v-if="open" :id="listId" class="m-0! mt-2 list-none divide-y divide-line overflow-hidden rounded-xl border border-line bg-inset p-0">
      <li
        v-for="(member, index) in visible"
        :key="start + index"
        :aria-posinset="start + index + 1"
        :aria-setsize="members.length"
        class="wait-member flex items-center gap-2.5 px-3 py-2"
        :class="{ 'opacity-60': finished && !member.outcome }"
        data-testid="wait-member"
        :aria-label="`${member.title}: ${status(member)}`"
      >
        <span class="grid size-5.5 shrink-0 place-items-center rounded-full" :class="glyphs[state(member)].class" aria-hidden="true">
          <Icon v-if="glyphs[state(member)].icon" :name="glyphs[state(member)].icon!" :size="12" />
        </span>
        <span class="min-w-0 flex-1" aria-hidden="true">
          <span class="flex min-w-0 items-center gap-1.5">
            <span class="truncate text-xs font-semibold text-ink">{{ member.title }}</span>
            <span v-for="detail in member.details.slice(0, 2)" :key="detail" class="shrink-0 rounded-full bg-hover px-1.5 font-mono text-[10px] text-muted">{{ detail }}</span>
            <span v-if="start + index === first" class="shrink-0 rounded-full bg-coral-soft px-1.5 text-[10px] font-bold text-coral">First</span>
          </span>
          <span class="block truncate text-2xs" :class="member.lastError && !member.outcome && live ? 'text-warning' : tones[state(member)]">{{ status(member) }}</span>
        </span>
        <RouterLink
          v-if="member.chatId || member.runId"
          :to="member.chatId ? `/chats/${member.chatId}` : `/runs/${member.runId}`"
          class="grid size-7 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-ink phone:size-10"
          :aria-label="`Open ${member.chatId ? 'task' : 'run'} ${member.details[0] ?? ''}`"
        >
          <Icon :name="ArrowUpRight" :size="14" />
        </RouterLink>
      </li>
    </ol>
    <nav v-if="open && pageCount > 1" class="mt-2 flex items-center justify-between gap-2 text-2xs text-muted" aria-label="Wait conditions">
      <button
        type="button"
        class="rounded-lg px-3 py-2 hover:bg-hover disabled:opacity-40"
        :disabled="page === 0"
        aria-label="Previous conditions"
        @click="selectedPage = page - 1"
      >
        Previous
      </button>
      <span class="tabular-nums" aria-live="polite">{{ start + 1 }}–{{ Math.min(start + pageSize, members.length) }} of {{ members.length }}</span>
      <button
        type="button"
        class="rounded-lg px-3 py-2 hover:bg-hover disabled:opacity-40"
        :disabled="page + 1 >= pageCount"
        aria-label="Next conditions"
        @click="selectedPage = page + 1"
      >
        Next
      </button>
    </nav>
  </div>
</template>

<style scoped>
/* A condition still awaited: a slow arc, quieter than a working agent. */
.wait-member-spinner {
  border: 2px solid var(--color-line);
  border-top-color: var(--color-accent);
  animation: wait-member-spin 1.6s linear infinite;
}
@keyframes wait-member-spin {
  to {
    transform: rotate(360deg);
  }
}

/* Its meter segment glows across while Cairn waits for it. */
.wait-member-shimmer {
  background: linear-gradient(
    90deg,
    color-mix(in srgb, var(--color-accent) 18%, transparent) 0%,
    color-mix(in srgb, var(--color-accent) 55%, transparent) 50%,
    color-mix(in srgb, var(--color-accent) 18%, transparent) 100%
  );
  background-size: 200% 100%;
  animation: wait-member-shimmer 2.4s ease-in-out infinite;
}
@keyframes wait-member-shimmer {
  from {
    background-position: 100% 0;
  }
  to {
    background-position: -100% 0;
  }
}

.wait-member {
  animation: wait-member-in 0.28s var(--ease-signal, ease-out) both;
}
@keyframes wait-member-in {
  from {
    opacity: 0;
    transform: translateY(3px);
  }
}

@media (prefers-reduced-motion: reduce) {
  .wait-member-spinner,
  .wait-member-shimmer,
  .wait-member {
    animation: none;
  }
  .wait-member-spinner {
    border-style: dashed;
  }
}
</style>
