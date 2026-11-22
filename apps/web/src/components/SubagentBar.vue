<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import {
  computed,
  inject,
  onBeforeUnmount,
  ref,
} from 'vue'
import {
  delegatedChatsKey,
  subagentDots as dots,
  subagentElapsed,
  subagentLabels,
  subagentState,
} from '../subagents'
import SubagentMark from './SubagentMark.vue'

// The subagents of the current request, beside the composer so they stay in sight while the
// conversation scrolls. Each one opens its full activity; on phones they fold into one button.
const props = defineProps<{ agents: ActivityArtifact[], active: boolean, selected?: string | null }>()
const emit = defineEmits<{ open: [id: string, opener: HTMLElement] }>()
const now = ref(Date.now())
const timer = setInterval(() => now.value = Date.now(), 1000)
onBeforeUnmount(() => clearInterval(timer))

const delegated = inject(delegatedChatsKey, null)
const state = (agent: ActivityArtifact) => subagentState(agent, props.active, delegated?.value)
const running = computed(() => props.agents.filter(agent => state(agent) === 'running'))
const first = computed(() => running.value[0] ?? props.agents[0])

// The queue can move a button between press and release, and the browser then sends the
// click elsewhere. A pointer press therefore opens on its release, which reaches the button:
// mouse and pen presses are captured here, touch presses implicitly by the browser.
// The slop is the pointer travel, in CSS pixels, that still counts as a press rather than a drag.
const pressSlop = 4
let press: {
  button: HTMLButtonElement
  pointerId: number
  x: number
  y: number
} | undefined
let released: HTMLButtonElement | undefined

function hold(event: PointerEvent) {
  released = undefined
  press = undefined
  if (!event.isPrimary || event.button !== 0)
    return

  const button = event.currentTarget as HTMLButtonElement
  press = {
    button,
    pointerId: event.pointerId,
    x: event.clientX,
    y: event.clientY,
  }
  if (event.pointerType !== 'touch')
    button.setPointerCapture(event.pointerId)
}

// A scroll or system gesture took the press over.
function cancel() {
  press = undefined
}

function release(id: string, event: PointerEvent) {
  const button = event.currentTarget as HTMLButtonElement
  const held = press
  press = undefined
  if (held?.button !== button || held.pointerId !== event.pointerId)
    return

  released = button
  // A drag must finish over the visible button because capture also retargets outside it.
  if (Math.hypot(event.clientX - held.x, event.clientY - held.y) > pressSlop
    && !button.contains(document.elementFromPoint(event.clientX, event.clientY))) {
    return
  }

  emit('open', id, button)
}

// The release decided; a tap's click would otherwise land on whatever now lies under it.
function dropTapClick(event: TouchEvent) {
  if (released === event.currentTarget && event.cancelable) {
    event.preventDefault()
    released = undefined
  }
}

// Keyboard and assistive clicks have no press of their own.
function openActivity(id: string, event: MouseEvent) {
  const button = event.currentTarget as HTMLButtonElement
  if (event.detail && released === button) {
    released = undefined
    return
  }

  emit('open', id, button)
}
</script>

<template>
  <section class="subagent-bar mb-2 flex min-w-0 items-center gap-2 rounded-2xl border border-line bg-surface px-2.5 py-1.5" aria-label="Subagents at work" data-testid="subagent-bar">
    <span class="shrink-0 pl-1 font-heading text-xs font-semibold text-ink phone:hidden">Subagents</span>
    <div class="flex min-w-0 flex-1 gap-1.5 overflow-x-auto [scrollbar-width:none] phone:hidden">
      <button
        v-for="agent in agents"
        :key="agent.id"
        class="inline-flex min-h-8 shrink-0 items-center gap-1.5 rounded-[10px] border py-1 pr-2 pl-1 text-xs hover:bg-hover"
        :class="selected === agent.id ? 'border-accent bg-soft' : 'border-line bg-inset'"
        :aria-label="`${agent.title}, ${subagentLabels[state(agent)]}. Open its activity`"
        :aria-pressed="selected === agent.id"
        data-testid="subagent-chip"
        @pointerdown="hold"
        @pointerup="release(agent.id, $event)"
        @pointercancel="cancel"
        @touchend="dropTapClick"
        @click="openActivity(agent.id, $event)"
      >
        <SubagentMark :agent="agent" :size="20" />
        <span class="max-w-40 truncate">{{ agent.title }}</span>
        <span class="size-1.5 shrink-0 rounded-full" :class="dots[state(agent)]" aria-hidden="true" />
        <span v-if="state(agent) === 'running'" class="tabular-nums text-muted" aria-hidden="true">{{ subagentElapsed(agent, active, now) }}</span>
      </button>
    </div>
    <span class="shrink-0 pr-1 text-2xs text-muted phone:hidden">{{ running.length === 1 ? '1 running' : `${running.length} running` }}</span>
    <button
      v-if="first"
      class="hidden min-h-11 min-w-0 flex-1 items-center gap-2 text-left phone:flex"
      :aria-label="`${running.length === 1 ? '1 subagent' : `${running.length} subagents`} working. Open their activity`"
      @pointerdown="hold"
      @pointerup="release(first.id, $event)"
      @pointercancel="cancel"
      @touchend="dropTapClick"
      @click="openActivity(first.id, $event)"
    >
      <span class="flex shrink-0 -space-x-1.5">
        <SubagentMark
          v-for="agent in agents.slice(0, 3)"
          :key="agent.id"
          :agent="agent"
          :size="22"
          class="ring-2 ring-surface"
        />
      </span>
      <span class="min-w-0 flex-1 truncate text-xs"><b class="font-semibold">{{ running.length === 1 ? '1 subagent' : `${running.length} subagents` }}</b> <span class="text-muted">working</span></span>
      <span class="subagent-pulse size-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true" />
    </button>
  </section>
</template>
