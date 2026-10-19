<script setup lang="ts">
import type { ActivityWait } from '../activity'
import type { LiveWait, WaitTone } from '../chat-wait'
import type { IconName } from '../icons'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import {
  observedTarget,
  recordedTitle,
  timeLeft,
  waitCountdown,
  waitDue,
  waited,
  waitOutcome,
  waitProgress,
  waitTarget,
  wakeTime,
} from '../chat-wait'
import {
  Bot,
  CircleCheck,
  CircleX,
  Clock,
  GitPullRequest,
  Hourglass,
  Pause,
  RefreshCw,
  TriangleAlert,
  Workflow,
} from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{
  wait: ActivityWait
  /** The conversation's pending wakeup; absent where it is unknown, as in a run's history. */
  live?: LiveWait
}>()
const emit = defineEmits<{ pause: [] }>()

const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | undefined
const wakeup = computed(() => props.wait.wakeup)
const state = computed(() => waitProgress(props.wait, props.live))
const current = computed(() => state.value.current)
const observation = computed(() => state.value.observation)
const finished = computed(() => state.value.finished)
const cancelled = computed(() => state.value.cancelled)
const paused = computed(() => state.value.paused)
const pausable = computed(() => state.value.pausable)
// Pending: the conversation still waits for this wakeup, with its deadline ahead.
const pending = computed(() => state.value.pending)
watch(pending, (value) => {
  clearInterval(clock)
  now.value = Date.now()
  if (value)
    clock = setInterval(() => now.value = Date.now(), 1000)
}, { immediate: true })
onBeforeUnmount(() => clearInterval(clock))

// Cairn updates the pending wakeup as it polls; the scheduling event keeps the original.
const polled = computed(() => current.value ? props.live!.wakeup! : wakeup.value)
const target = computed(() => wakeup.value ? waitTarget(wakeup.value, now.value) : null)
const icons: Record<string, IconName> = {
  timer: Hourglass,
  pr: GitPullRequest,
  workflow: Workflow,
  run: Bot,
}

// Why the agent waits, shown apart from the status: a long one is clamped so the card stays small.
const reason = computed(() => wakeup.value?.reason ?? props.wait.reason ?? '')
const reasonText = ref<HTMLElement>()
const expanded = ref(false)
const clamped = ref(false)
const observer = typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(measure)

function measure() {
  const element = reasonText.value
  clamped.value = !!element && !expanded.value && element.scrollHeight > element.clientHeight + 1
}

watch(reasonText, (element) => {
  observer?.disconnect()
  if (element)
    observer?.observe(element)
}, { flush: 'post' })
watch(reason, () => nextTick(measure))
onBeforeUnmount(() => observer?.disconnect())

const view = computed<{
  tone: WaitTone | 'live' | 'paused'
  icon: IconName
  title: string
  detail: string
}>(() => {
  if (finished.value) {
    if (!wakeup.value?.condition && !observation.value) {
      return {
        tone: 'neutral',
        icon: Clock,
        title: 'Resumed as scheduled',
        detail: duration(),
      }
    }

    const outcome = waitOutcome(observation.value)
    const icon = outcome.tone === 'ok' ? CircleCheck : outcome.tone === 'error' ? CircleX : outcome.tone === 'warning' ? TriangleAlert : Clock
    return {
      tone: outcome.tone,
      icon,
      title: outcome.title,
      // Like a pending one, a finished card says why it waited, above this.
      detail: [awaited(), outcome.detail, resolution()].filter(Boolean).join(' · '),
    }
  }

  if (props.wait.ended === 'replaced') {
    return {
      tone: 'paused',
      icon: Hourglass,
      title: 'Replaced by a later wait call',
      detail: '',
    }
  }

  if (cancelled.value) {
    return {
      tone: 'paused',
      icon: Hourglass,
      title: 'Wait cancelled',
      detail: '',
    }
  }

  if (paused.value) {
    return {
      tone: 'paused',
      icon: Pause,
      title: target.value!.title,
      detail: '',
    }
  }

  // A run's history cannot tell whether the wait still stands: it is shown as recorded.
  return {
    tone: props.live ? 'live' : 'neutral',
    icon: icons[target.value!.kind]!,
    title: props.live ? target.value!.title : recordedTitle(wakeup.value!, now.value),
    detail: '',
  }
})

/** A finished card still names what it waited for; a timer's outcome says it already. */
function awaited() {
  if (!wakeup.value)
    return observedTarget(observation.value)
  return wakeup.value.condition ? target.value!.short : ''
}

/** After its result: how the conversation went on, or why it did not. */
function resolution() {
  if (!wakeup.value)
    return 'The result was already available'
  if (props.wait.resumedAt)
    return duration()
  if (props.wait.ended === 'replaced')
    return 'Replaced by a later wait call'
  if (cancelled.value)
    return 'Wait cancelled'
  if (paused.value)
    return 'Resumes when you resume the conversation'
  return 'Resuming when capacity allows'
}

function duration() {
  const start = wakeup.value?.createdAt
  return start && props.wait.resumedAt ? `Waited ${waited(props.wait.resumedAt - start)}` : ''
}

const chips = computed(() => !finished.value && !cancelled.value && !state.value.replaced ? target.value?.details.filter(Boolean) ?? [] : [])
const checked = computed(() => {
  const next = polled.value?.nextCheckAt
  // Cairn checks GitHub only once the turn has ended, and until the deadline.
  if (!pending.value || !props.live?.settled || waitDue(polled.value!, now.value) || !polled.value?.condition || polled.value.condition.type === 'run_finished')
    return ''
  if (polled.value.lastError)
    return ''
  return next && next > now.value ? `Next check in ${timeLeft(next - now.value)}` : 'Checking…'
})
const progress = computed(() => {
  const value = wakeup.value
  if (!value || !pending.value)
    return 0
  const total = value.wakeAt - value.createdAt
  return total > 0 ? Math.min(100, Math.max(0, ((now.value - value.createdAt) / total) * 100)) : 100
})
const tones: Record<string, string> = {
  live: 'bg-soft text-accent',
  paused: 'bg-hover text-muted',
  neutral: 'bg-hover text-muted',
  ok: 'bg-success/12 text-success',
  error: 'bg-danger-surface text-danger',
  warning: 'bg-warning-surface text-warning',
}
</script>

<template>
  <article class="chat-wait my-3 overflow-hidden rounded-xl border border-line bg-surface" :aria-label="view.title">
    <div class="flex items-start gap-3 px-3.5 py-3">
      <span class="relative grid size-8 shrink-0 place-items-center rounded-lg" :class="tones[view.tone]">
        <Icon :name="view.icon" :size="16" />
        <span v-if="pending" class="live-dot absolute -right-0.5 -top-0.5" />
      </span>
      <div class="min-w-0 flex-1">
        <p class="m-0! text-sm font-semibold leading-snug text-ink">
          {{ view.title }}
        </p>
        <template v-if="reason">
          <p ref="reasonText" class="m-0! mt-0.5 text-xs text-muted wrap-anywhere whitespace-pre-line" :class="{ 'line-clamp-3': !expanded }">
            {{ reason }}
          </p>
          <button
            v-if="clamped || expanded"
            type="button"
            class="mt-0.5 text-xs font-semibold text-accent hover:underline"
            :aria-expanded="expanded"
            @click="expanded = !expanded; nextTick(measure)"
          >
            {{ expanded ? 'Show less' : 'Show more' }}
          </button>
        </template>
        <p v-if="view.detail" class="m-0! mt-0.5 text-xs text-muted wrap-anywhere">
          {{ view.detail }}
        </p>
        <p v-if="chips.length || checked || (pending && polled?.lastError)" class="m-0! mt-2 flex flex-wrap gap-1.5 text-2xs">
          <span v-for="chip in chips" :key="chip" class="rounded-full bg-hover px-2 py-0.5 font-mono text-muted">{{ chip }}</span>
          <span v-if="pending && polled?.lastError" class="flex items-center gap-1 rounded-full bg-warning-surface px-2 py-0.5 font-semibold text-warning" :title="polled.lastError">
            <Icon :name="TriangleAlert" :size="11" />GitHub unreachable · retrying
          </span>
          <span v-else-if="checked" class="flex items-center gap-1 rounded-full bg-soft px-2 py-0.5 font-semibold text-accent">
            <Icon :name="RefreshCw" :size="11" />{{ checked }}
          </span>
        </p>
      </div>
      <div v-if="pending && wakeup" class="shrink-0 text-right">
        <p class="m-0! font-heading text-base font-bold tabular-nums leading-tight text-accent">
          {{ waitCountdown(wakeup, now) }}
        </p>
        <!-- A timer names its time in the title already. -->
        <p v-if="wakeup.condition" class="m-0! mt-0.5 text-2xs text-subtle">
          deadline {{ wakeTime(wakeup.wakeAt, now) }}
        </p>
      </div>
      <div v-else-if="paused && !finished && wakeup" class="shrink-0 text-right">
        <p class="m-0! text-sm font-semibold text-muted">
          Paused
        </p>
        <p v-if="wakeup.condition" class="m-0! mt-0.5 text-2xs text-subtle">
          deadline {{ wakeTime(wakeup.wakeAt, now) }}
        </p>
      </div>
      <p v-else-if="!finished && !live && wakeup?.condition" class="m-0! shrink-0 text-2xs text-subtle">
        Deadline {{ wakeTime(wakeup.wakeAt, now) }}
      </p>
    </div>
    <div v-if="pausable" class="-mt-1 flex justify-end px-3 pb-2.5">
      <button
        type="button"
        class="flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-semibold text-muted hover:bg-hover hover:text-ink disabled:opacity-50"
        :disabled="live?.busy"
        @click="emit('pause')"
      >
        <Icon :name="Pause" :size="13" />Pause wait
      </button>
    </div>
    <div v-if="pending" class="h-[3px] bg-hover" aria-hidden="true">
      <i class="block h-full rounded-r-full bg-accent transition-[width] duration-1000 ease-linear" :style="{ width: `${progress}%` }" />
    </div>
  </article>
</template>
