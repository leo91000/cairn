<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import type { Box } from '../press'
import {
  computed,
  inject,
  onBeforeUnmount,
  ref,
} from 'vue'
import {
  releasedOnControl,
  sameBox,
  stopsRowFling,
  touchSlop,
  visibleBox,
} from '../press'
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

// A queued message can appear between press and release and move these buttons. The browser
// then sends the click to whatever moved under the pointer, so pointer presses open on release.
let press: {
  id: string
  button: HTMLButtonElement
  pointerId: number
  box: Box
} | undefined
// Not cleared on unmount: opening can navigate away before the browser's click arrives.
let stopWatching: (() => void) | undefined
onBeforeUnmount(() => cancelPress())

function shownBox(button: HTMLButtonElement) {
  return visibleBox(button.getBoundingClientRect(), button.parentElement?.getBoundingClientRect())
}

let rowScrolledAt = Number.NEGATIVE_INFINITY

function rowScrolled(event: Event) {
  rowScrolledAt = event.timeStamp
}

function holdPress(id: string, event: PointerEvent, inScrollingRow = false) {
  if (!event.isPrimary || event.button !== 0)
    return
  if (inScrollingRow && stopsRowFling(event.pointerType, event.timeStamp, rowScrolledAt))
    return

  cancelPress()
  const button = event.currentTarget as HTMLButtonElement
  press = {
    id,
    button,
    pointerId: event.pointerId,
    box: shownBox(button),
  }
  // A mouse has no implicit capture: without it, the release and click of a moved button go
  // to whatever moved under the pointer. Touch and pen keep the browser's own targeting.
  if (event.pointerType === 'mouse')
    button.setPointerCapture(event.pointerId)
  // The release may reach what moved under the pointer, as WebKit does even for touch.
  window.addEventListener('pointerup', releasePress, true)
  // A later press means this one ended without a release this bar saw.
  window.addEventListener('pointerdown', cancelPress, true)
}

function releasePress(event: PointerEvent) {
  const held = press
  if (held?.pointerId !== event.pointerId)
    return

  cancelPress()
  const { id, button } = held
  const current = shownBox(button)
  // An unmoved touch or pen press stays the browser's own tap, as before: its click opens the
  // activity, and the activity's focus then follows the pointer.
  if (event.pointerType !== 'mouse' && sameBox(held.box, current)) {
    guardNativeTap(id, button, current)
    return
  }

  ignoreFollowingClick(button)
  const slop = event.pointerType === 'mouse' ? 0 : touchSlop
  if (!releasedOnControl(held.box, current, event.clientX, event.clientY, slop))
    return

  // The browser's own mouse-down may land beside a button that moved, so focus it here.
  if (event.pointerType !== 'mouse')
    button.focus({ preventScroll: true, focusVisible: false })
  emit('open', id, button)
}

function cancelPress() {
  press = undefined
  window.removeEventListener('pointerup', releasePress, true)
  window.removeEventListener('pointerdown', cancelPress, true)
}

// The release decided; the compatibility mouse events and click that follow may target
// another control, or move focus inside the activity that just opened.
function ignoreFollowingClick(button: HTMLButtonElement) {
  // A touch's mouse-down on the unmoved button still focuses it, as before.
  watchFollowingClick(event => !(event.type === 'mousedown' && button.contains(event.target as Node)))
}

// The browser taps an unmoved button after the release. If a queued message moves the button
// first, that tap would land on the row instead: open the activity and ignore it.
function guardNativeTap(id: string, button: HTMLButtonElement, released: Box) {
  let opened = false
  watchFollowingClick(() => {
    if (!opened && sameBox(released, shownBox(button)))
      return false

    if (!opened) {
      opened = true
      button.focus({ preventScroll: true, focusVisible: false })
      emit('open', id, button)
    }

    return true
  })
}

const followingEvents = ['mousedown', 'mouseup', 'click'] as const

// Watches the compatibility mouse events and click the browser sends after a release, ignoring
// those [ignore] picks, until that click, the next press or a second has passed.
function watchFollowingClick(ignore: (event: MouseEvent) => boolean) {
  stopWatching?.()
  const watch = (event: MouseEvent) => {
    // Keyboard and assistive activation report no click count.
    if (event.detail === 0)
      return

    if (ignore(event)) {
      event.preventDefault()
      event.stopImmediatePropagation()
    }

    if (event.type === 'click')
      stop()
  }

  // A new press always comes after this press's own mouse events and click, which some
  // browsers skip entirely.
  const next = () => stop()
  const timer = setTimeout(stop, 1000)

  function stop() {
    clearTimeout(timer)
    for (const type of followingEvents)
      window.removeEventListener(type, watch, true)
    window.removeEventListener('pointerdown', next, true)
    stopWatching = undefined
  }

  for (const type of followingEvents)
    window.addEventListener(type, watch, true)
  window.addEventListener('pointerdown', next, true)
  stopWatching = stop
}

// Keyboard, assistive and other non-primary activation; primary presses open on release.
function openFromClick(id: string, event: MouseEvent) {
  emit('open', id, event.currentTarget as HTMLElement)
}
</script>

<template>
  <section class="subagent-bar mb-2 flex min-w-0 items-center gap-2 rounded-2xl border border-line bg-surface px-2.5 py-1.5" aria-label="Subagents at work" data-testid="subagent-bar">
    <span class="shrink-0 pl-1 font-heading text-xs font-semibold text-ink phone:hidden">Subagents</span>
    <div class="flex min-w-0 flex-1 gap-1.5 overflow-x-auto [scrollbar-width:none] phone:hidden" @scroll="rowScrolled">
      <button
        v-for="agent in agents"
        :key="agent.id"
        class="inline-flex min-h-8 shrink-0 items-center gap-1.5 rounded-[10px] border py-1 pr-2 pl-1 text-xs hover:bg-hover"
        :class="selected === agent.id ? 'border-accent bg-soft' : 'border-line bg-inset'"
        :aria-label="`${agent.title}, ${subagentLabels[state(agent)]}. Open its activity`"
        :aria-pressed="selected === agent.id"
        data-testid="subagent-chip"
        @pointerdown="holdPress(agent.id, $event, true)"
        @pointercancel="cancelPress"
        @lostpointercapture="cancelPress"
        @contextmenu="cancelPress"
        @click="openFromClick(agent.id, $event)"
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
      @pointerdown="holdPress(first.id, $event)"
      @pointercancel="cancelPress"
      @lostpointercapture="cancelPress"
      @contextmenu="cancelPress"
      @click="openFromClick(first.id, $event)"
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
