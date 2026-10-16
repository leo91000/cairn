<script setup lang="ts">
import type { ChatWakeup } from '../../../../packages/contracts/chats'
import type { ActivityWait } from '../activity'
import type { WaitTone } from '../chat-wait'
import type { IconName } from '../icons'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import {
  timeLeft,
  waitCountdown,
  waited,
  waitOutcome,
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
  live?: { wakeup: ChatWakeup | null, paused: boolean }
}>()
const emit = defineEmits<{ pause: [] }>()

const now = ref(Date.now())
let clock: ReturnType<typeof setInterval> | undefined
const wakeup = computed(() => props.wait.wakeup)
const finished = computed(() => !!props.wait.resumedAt || props.wait.observation !== undefined)
const current = computed(() => !!wakeup.value && props.live?.wakeup?.id === wakeup.value.id)
// Pending: the conversation still waits for this wakeup, with its deadline ahead.
const pending = computed(() => current.value && !props.live?.paused && !finished.value)
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

const view = computed<{
  tone: WaitTone | 'live' | 'paused'
  icon: IconName
  title: string
  detail: string
}>(() => {
  const reason = wakeup.value?.reason ?? ''
  // An observed condition may still be cancelled or paused before the wakeup launches.
  const interrupted = !props.wait.resumedAt && !!wakeup.value && (
    !!props.wait.ended || (props.live && (!current.value || props.live.paused))
  )
  if (finished.value && !interrupted) {
    if (!wakeup.value?.condition && !props.wait.observation) {
      return {
        tone: 'neutral',
        icon: Clock,
        title: 'Resumed as scheduled',
        detail: duration(),
      }
    }

    const outcome = waitOutcome(props.wait.observation)
    const icon = outcome.tone === 'ok' ? CircleCheck : outcome.tone === 'error' ? CircleX : outcome.tone === 'warning' ? TriangleAlert : Clock
    const when = !wakeup.value ? 'The result was already available' : props.wait.resumedAt ? duration() : 'Resuming when capacity allows'
    return {
      tone: outcome.tone,
      icon,
      title: outcome.title,
      detail: [outcome.detail, when].filter(Boolean).join(' · '),
    }
  }

  if (props.wait.ended === 'message') {
    return {
      tone: 'paused',
      icon: Hourglass,
      title: 'Wait cancelled by your message',
      detail: reason,
    }
  }

  if (props.wait.ended === 'replaced') {
    return {
      tone: 'paused',
      icon: Hourglass,
      title: 'Replaced by a new wait',
      detail: reason,
    }
  }

  if (props.live && !current.value) {
    return {
      tone: 'paused',
      icon: Hourglass,
      title: 'Wait cancelled',
      detail: reason,
    }
  }

  if (current.value && props.live?.paused) {
    return {
      tone: 'paused',
      icon: Pause,
      title: 'Wait paused',
      detail: 'The deadline still applies. Resume the conversation to continue.',
    }
  }

  return {
    tone: 'live',
    icon: icons[target.value!.kind]!,
    title: target.value!.title,
    detail: reason,
  }
})

function duration() {
  const start = wakeup.value?.createdAt
  return start && props.wait.resumedAt ? `Waited ${waited(props.wait.resumedAt - start)}` : ''
}

const chips = computed(() => {
  if (!pending.value && view.value.tone !== 'live')
    return []
  return target.value?.details.filter(Boolean) ?? []
})
const checked = computed(() => {
  const next = polled.value?.nextCheckAt
  if (!pending.value || !polled.value?.condition || polled.value.condition.type === 'run_finished')
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
  <section class="chat-wait my-3 overflow-hidden rounded-xl border border-line bg-surface" :aria-label="view.title">
    <div class="flex items-start gap-3 px-3.5 py-3">
      <span class="relative grid size-8 shrink-0 place-items-center rounded-lg" :class="tones[view.tone]">
        <Icon :name="view.icon" :size="16" />
        <span v-if="pending" class="live-dot absolute -right-0.5 -top-0.5" />
      </span>
      <div class="min-w-0 flex-1">
        <p class="m-0! text-sm font-semibold leading-snug text-ink">
          {{ view.title }}
        </p>
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
        <p class="m-0! mt-0.5 text-2xs text-subtle">
          {{ wakeup.condition ? `deadline ${wakeTime(wakeup.wakeAt, now)}` : wakeTime(wakeup.wakeAt, now) }}
        </p>
      </div>
      <p v-else-if="!finished && !live && wakeup" class="m-0! shrink-0 text-2xs text-subtle">
        {{ wakeup.condition ? 'Deadline' : 'Resumes' }} {{ wakeTime(wakeup.wakeAt, now) }}
      </p>
    </div>
    <div v-if="pending" class="-mt-1 flex justify-end px-3 pb-2.5">
      <button type="button" class="flex items-center gap-1.5 rounded-lg px-2.5 py-1 text-xs font-semibold text-muted hover:bg-hover hover:text-ink" @click="emit('pause')">
        <Icon :name="Pause" :size="13" />Pause wait
      </button>
    </div>
    <div v-if="pending" class="h-[3px] bg-hover" aria-hidden="true">
      <i class="block h-full rounded-r-full bg-accent transition-[width] duration-1000 ease-linear" :style="{ width: `${progress}%` }" />
    </div>
  </section>
</template>
