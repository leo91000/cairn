<script setup lang="ts">
import type { Deliverable } from '../../../../packages/contracts/artifacts'
import type { ChatWakeup } from '../../../../packages/contracts/chats'
import type { RunEvent, TaskOutcome } from '../../../../packages/contracts/contracts'
import type { ActivityEntry } from '../activity'
import type { SendingMessage } from '../chat-delivery'
import type { ChatQuota } from '../chat-quota'
import type { LiveWait } from '../chat-wait'
import type { ReadingPosition } from '../history-cache'
import { twMerge } from 'tailwind-merge'
import {
  computed,
  nextTick,
  onBeforeMount,
  onBeforeUnmount,
  onBeforeUpdate,
  ref,
  shallowRef,
  watch,
} from 'vue'
import { latestArtifacts } from '../../../../packages/contracts/artifacts'
import { activityEntries, ActivityGroups } from '../activity'
import { currentPlan } from '../agent-plan'
import { quotaEntries } from '../chat-quota'
import { deliveryEntries } from '../deliverables'
import { ArrowDown, Maximize2, Minimize2 } from '../icons'
import { MessageTimes } from '../message-times'
import { exclusiveSuspensionLabel } from '../run-status'
import { backgroundWait, waitingStep, workingStep } from '../signal'
import { mentionSegments } from '../skill-mentions'
import { currentSubagents, subagentTeams, waitingForSubagents } from '../subagents'
import { iconButton } from '../ui'
import ActivityContent from './ActivityContent.vue'
import AgentActions from './AgentActions.vue'
import ArtifactRail from './ArtifactRail.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import ChatNotice from './ChatNotice.vue'
import ChatOutcome from './ChatOutcome.vue'
import ChatQuotaCard from './ChatQuotaCard.vue'
import ChatWaitCard from './ChatWaitCard.vue'
import Icon from './Icon.vue'
import MessageAttachments from './MessageAttachments.vue'
import PlanCard from './PlanCard.vue'
import PlanUpdate from './PlanUpdate.vue'
import SubagentTeam from './SubagentTeam.vue'
import UiButton from './UiButton.vue'
import WorkingIndicator from './WorkingIndicator.vue'

const props = defineProps<{
  events: RunEvent[]
  active: boolean
  suspended?: boolean
  agent: string
  agentId?: string
  task: string
  more: boolean
  loading: boolean
  trimmed: number
  compactToolbar?: boolean
  chat?: boolean
  outcome?: TaskOutcome | null
  deliverables?: Deliverable[]
  sending?: SendingMessage[]
  cacheKey?: string
  position?: ReadingPosition
  loadingOlder?: boolean
  olderError?: string
  skills?: string[]
  /** The page shows the current subagents beside the composer, so the feed need not. */
  subagentBar?: boolean
  /** The conversation's pending wakeup, which keeps its wait card live. */
  wakeup?: LiveWait
  quota?: ChatQuota | null
  canManageAccounts?: boolean
  busy?: boolean
}>()
const emit = defineEmits<{
  load: []
  position: [value: ReadingPosition, key?: string]
  pauseWait: []
  resumeQuota: []
}>()
const skillNames = computed(() => new Set(props.skills ?? []))
const messageTimes = new MessageTimes()
onBeforeMount(() => messageTimes.beginRender())
onBeforeUpdate(() => messageTimes.beginRender())
const activityGroups = shallowRef(new ActivityGroups())
watch([() => props.cacheKey, () => props.chat, () => props.events[0]?.runId], () => {
  activityGroups.value = new ActivityGroups()
}, { flush: 'sync' })
// Keeps its identity while the wakeup does, so a new conversation snapshot does not rebuild the feed.
const pendingWakeup = computed<ChatWakeup | null>((previous) => {
  const next = props.wakeup?.wakeup ?? null
  return previous && next?.id === previous.id && JSON.stringify(next.observation) === JSON.stringify(previous.observation) ? previous : next
})
const entries = computed(() => {
  const projected = activityEntries(props.events, props.chat, activityGroups.value, pendingWakeup.value)
  const entries = props.chat ? quotaEntries(projected, props.events) : projected

  const acknowledged = new Set(props.events.filter(event => event.type === 'chat.user').map(event => event.payload?.messageId))
  for (const { message, label } of props.sending ?? []) {
    if (!acknowledged.has(message.id)) {
      entries.push({
        kind: 'message',
        role: 'user',
        id: `sending:${message.id}`,
        time: message.createdAt,
        text: message.text,
        attachments: message.attachments,
        delivery: label,
      })
    }
  }

  return deliveryEntries(entries, props.deliverables ?? [])
})
const teams = computed(() => subagentTeams(entries.value))
const subagents = computed(() => currentSubagents(entries.value))
const activity = computed(() => entries.value.filter((entry): entry is ActivityEntry => entry.kind !== 'deliverables'))
const plan = computed(() => currentPlan(activity.value))
// Only the latest plan is still being followed.
const livePlan = computed(() => props.active && !props.suspended ? activity.value.findLast(entry => entry.kind === 'plan')?.id : undefined)
const working = computed(() => {
  const wait = backgroundWait(props.events)
  if (wait)
    return { ...waitingStep(wait), subagents: waitingForSubagents(props.events, subagents.value) }
  return workingStep(activity.value, props.agent, props.events[0]?.createdAt ?? null)
})
const visibleOutcome = computed(() => !props.active && !props.sending?.length && !props.loading ? props.outcome : null)
const outcomeEntryId = computed(() => {
  const items = entries.value
  let index = items.findLastIndex(entry => entry.kind === 'message')
  const last = items[index]
  if (!last || (last.kind === 'message' && last.role === 'user'))
    return null
  if (items[index + 1]?.kind === 'deliverables')
    index++
  return items[index]!.id
})
const artifactViewer = ref<string | null>(null)
const follow = ref(props.position?.follow ?? true)
const fullscreen = ref(false)
const viewer = ref<HTMLDialogElement>()
const fullscreenHost = ref<HTMLElement>()
const fullscreenButton = ref<HTMLButtonElement>()
const scroller = ref<HTMLElement>()
const conversation = ref<HTMLElement>()
const atStart = ref(true)
const scrolling = ref(false)
const atEnd = ref(true)
// The way back stays mounted while paused, so a scroll (including an older page
// anchoring the reader) only fades it out and never takes its keyboard focus.
const latestFocused = ref(false)
const showLatest = computed(() => !follow.value && !atEnd.value)
// Resuming removes it: a keyboard user's focus moves to the reader first. A tap
// in Safari never focuses the button, so the composer keeps its keyboard. WebKit
// also fires no blur for a removed element, which would leave latestFocused set.
watch(showLatest, (shown) => {
  if (shown || !latestFocused.value)
    return
  latestFocused.value = false
  scroller.value?.focus({ preventScroll: true })
})
let scrollEnd: ReturnType<typeof setTimeout> | undefined
let pagingFrame: number | undefined
let prependPending = false
let resizeObserver: ResizeObserver | undefined
// The browser keeps the text under the reader in place while content changes above or
// below it (scroll anchoring); following pins the end after every change of size.
const anchoring = CSS.supports('overflow-anchor', 'auto')
// The position the feed set itself, whose scroll event is not the reader's.
let assigned: number | undefined
let lastTop = 0
// Opening shows the conversation first, filling only the view and one screen above it.
// Once the browser is idle, older pages keep Android's three-screen buffer ahead of the
// reader; anchoring keeps them out of sight.
const settled = ref(false)
// The reader scrolled up by themselves: the history banner may tell them to wait.
const reading = ref(false)
const idle = window.requestIdleCallback ?? ((callback: () => void) => setTimeout(callback, 200))
const cancelIdle = window.cancelIdleCallback ?? clearTimeout
const settling = idle(() => settled.value = true, { timeout: 1000 })

function setScroll(el: HTMLElement, top: number) {
  el.scrollTop = top
  assigned = el.scrollTop
  lastTop = assigned
}

// Runs after layout and before paint, so following never shows an intermediate frame.
function stick() {
  const el = scroller.value
  if (el && follow.value)
    setScroll(el, el.scrollHeight)
}

watch([scroller, conversation], ([element, content]) => {
  resizeObserver?.disconnect()
  if (!element)
    return
  follow.value = props.position?.follow ?? true
  if (follow.value)
    stick()
  else if (props.position)
    setScroll(element, props.position.top)
  // A reader who left older messages open keeps reading there.
  reading.value = !follow.value
  resizeObserver = new ResizeObserver(() => {
    stick()
    measureEnd()
    schedulePaging()
  })
  resizeObserver.observe(element)
  if (content)
    resizeObserver.observe(content)
  schedulePaging()
})
onBeforeUnmount(() => {
  savePosition()
  resizeObserver?.disconnect()
  cancelIdle(settling)
  clearTimeout(scrollEnd)
  if (pagingFrame !== undefined)
    cancelAnimationFrame(pagingFrame)
})

// As Android's canScrollForward: a resize can leave nothing below without a scroll event.
function measureEnd() {
  const el = scroller.value
  if (el)
    atEnd.value = el.scrollHeight - el.scrollTop - el.clientHeight < 2
}

function savePosition() {
  if (scroller.value)
    emit('position', { top: scroller.value.scrollTop, follow: follow.value }, props.cacheKey)
}

function followLatest() {
  follow.value = true
  stick()
  savePosition()
}

function loadOlder() {
  if (!props.more || props.loading || props.loadingOlder || prependPending)
    return
  prependPending = true
  emit('load')
}

watch([() => props.events, () => props.loadingOlder], async () => {
  if (props.loadingOlder || !prependPending)
    return
  // Anchoring keeps the reader's text in place, except at the very top, where browsers
  // never anchor, and in engines without it. Measure just before the page renders: the
  // reader may have kept scrolling while the response was in flight.
  const el = scroller.value
  const anchor = el && !follow.value && (!anchoring || el.scrollTop < 1) && { height: el.scrollHeight, top: el.scrollTop }
  await nextTick()
  if (el && anchor && !props.olderError)
    setScroll(el, anchor.top + el.scrollHeight - anchor.height)

  prependPending = false
  savePosition()
  schedulePaging()
})

function schedulePaging() {
  if (pagingFrame !== undefined)
    return
  pagingFrame = requestAnimationFrame(() => {
    pagingFrame = undefined
    const el = scroller.value
    if (!el || el.clientHeight === 0)
      return
    atStart.value = el.scrollTop < 1
    // Pages can fold into only a few rows. Following new output is independent from history.
    const buffer = settled.value || reading.value ? 3 * el.clientHeight : el.clientHeight
    if (el.scrollTop < buffer && !props.olderError)
      loadOlder()
  })
}

watch([entries, settled, () => props.more, () => props.loading, () => props.olderError], schedulePaging, { flush: 'post' })

// As on Android, scrolling up pauses following and scrolling back to the end resumes it.
// Positions the feed sets itself, and the browser's anchoring, change neither.
function scrolled() {
  const el = scroller.value
  if (!el)
    return
  const top = el.scrollTop
  const own = assigned !== undefined && Math.abs(top - assigned) < 1
  assigned = undefined
  if (!own) {
    const distance = el.scrollHeight - top - el.clientHeight
    if (distance <= 60) {
      follow.value = true
    }
    else if (top < lastTop - 1) {
      follow.value = false
      reading.value = true
    }
  }

  lastTop = top
  measureEnd()
  scrolling.value = true
  clearTimeout(scrollEnd)
  scrollEnd = setTimeout(() => scrolling.value = false, 150)
  schedulePaging()
  savePosition()
}

let fullscreenReturnFocus: HTMLElement | null = null

async function enterFullscreen() {
  fullscreenReturnFocus = document.activeElement as HTMLElement | null
  viewer.value?.showModal()
  fullscreen.value = true
  await nextTick()
  fullscreenButton.value?.focus()
  stick()
}

async function exitFullscreen() {
  fullscreen.value = false
  await nextTick()
  viewer.value?.close()
  // WebKit releases the dialog's inert state on the next rendering frame.
  await new Promise(requestAnimationFrame)
  if (fullscreenButton.value?.getClientRects().length)
    fullscreenButton.value.focus()
  else
    fullscreenReturnFocus?.focus()
}

onBeforeUnmount(() => viewer.value?.close())
defineExpose({
  teams,
  subagents,
  plan,
  openFiles: () => { artifactViewer.value = '' },
  enterFullscreen,
  followLatest,
})
</script>

<template>
  <div class="activity-mount flex flex-1 min-h-0 flex-col">
    <Teleport to="body">
      <dialog
        ref="viewer"
        class="activity-fullscreen fixed [inset:0] w-full max-w-none h-dvh max-h-none border-0 bg-raised text-ink p-0 m-0"
        aria-label="Fullscreen activity"
        @cancel.prevent="exitFullscreen"
      >
        <div ref="fullscreenHost" class="activity-fullscreen-host h-full rounded-[0]" />
      </dialog>
    </Teleport>
    <Teleport :to="fullscreenHost || 'body'" :disabled="!fullscreen">
      <section class="activity-feed flex flex-1 min-h-0 min-w-0 flex-col overflow-hidden bg-transparent" :class="{ 'is-fullscreen': fullscreen }" aria-label="Run conversation">
        <header v-if="!compactToolbar || fullscreen" :class="chat ? ['py-0! phone:py-0!', { 'phone:hidden!': !fullscreen }] : ''" class="activity-toolbar flex justify-between items-center gap-4.5 bg-transparent shrink-0 phone:gap-2.5 phone:flex-wrap px-6 py-[17px] phone:px-[15px] phone:py-[13px]">
          <div v-if="!chat" class="activity-toolbar-title flex items-center gap-[11px] font-semibold text-sm min-w-0 phone:[flex:1_1_160px]">
            <span class="activity-presence w-[7px] h-[7px] rounded-full bg-[light-dark(#8e8baa,_var(--dark-accent-surface))] shrink-0" :class="{ live: active && !suspended }" /><span>{{ active && suspended ? exclusiveSuspensionLabel : fullscreen ? task : chat ? (active ? 'Working' : 'Conversation') : 'Agent activity' }}</span>
          </div>
          <div class="activity-toolbar-controls ml-auto flex items-center gap-5 shrink-0 phone:flex-1 phone:justify-end phone:gap-4">
            <button v-if="deliverables?.length" class="rounded-md px-2 py-2 text-xs text-muted hover:bg-hover hover:text-ink" @click="artifactViewer = ''">
              Files · {{ latestArtifacts(deliverables).length }}
            </button>
            <button
              ref="fullscreenButton"
              :class="twMerge(iconButton, 'icon-button')"
              :aria-label="fullscreen ? 'Exit fullscreen' : 'Open activity fullscreen'"
              @click="fullscreen ? exitFullscreen() : enterFullscreen()"
            >
              <Icon v-if="fullscreen" :name="Minimize2" :size="19" /><Icon v-else :name="Maximize2" :size="19" />
            </button>
          </div>
        </header>
        <div class="relative flex flex-1 min-h-0 flex-col">
          <div v-if="olderError || (loadingOlder && atStart && reading)" role="status" class="absolute top-2 left-1/2 -translate-x-1/2 z-10 flex items-center gap-2 rounded-xl bg-raised px-3 py-2 text-xs shadow-md max-w-[90%]">
            <template v-if="olderError">
              <span class="text-danger">{{ olderError }}</span>
              <UiButton class="text-xs" :disabled="loadingOlder" @click="loadOlder">
                Retry
              </UiButton>
            </template>
            <span v-else>Loading history…</span>
          </div>
          <div
            ref="scroller"
            class="activity-scroll flex flex-col flex-1 min-h-0 overflow-auto overscroll-contain [scrollbar-width:thin] [scrollbar-color:var(--color-control)_transparent] focus-visible:outline-2 focus-visible:outline-offset-[-3px] focus-visible:outline-accent"
            tabindex="0"
            role="region"
            aria-label="Activity output"
            @scroll="scrolled"
          >
            <!-- As on Android, a short conversation sits at the bottom, next to the composer or the latest activity. -->
            <div ref="conversation" class="activity-conversation mt-auto w-full max-w-205 pt-5 pb-7 px-9 mx-auto my-0 phone:px-4 phone:py-6">
              <p v-if="trimmed" class="activity-retention text-2xs text-muted leading-[1.8]">
                Showing the latest {{ events.length.toLocaleString() }} events. {{ trimmed.toLocaleString() }} earlier events are outside this view.
              </p>
              <template v-for="entry in entries" :key="entry.id">
                <div v-if="entry.kind === 'deliverables'" class="my-3">
                  <ArtifactRail :items="entry.files" @open="artifactViewer = $event.id" />
                </div>
                <!-- Conversations and runs follow the Android reader: a tonal bubble for you, plain text for the agent. -->
                <!-- As on Android, attachments sit above the bubble, right-aligned and outside it. -->
                <article v-else-if="entry.kind === 'message' && entry.role === 'user'" class="activity-message chat-message ml-auto! my-3 flex w-fit max-w-[90%] flex-col items-end gap-1.5">
                  <span class="sr-only">You</span>
                  <MessageAttachments v-if="entry.attachments?.length" :attachments="entry.attachments" />
                  <div v-if="entry.text" class="user-bubble w-fit max-w-full rounded-2xl rounded-br-sm bg-variant px-4 py-3">
                    <p class="whitespace-pre-wrap text-lg leading-normal">
                      <template v-for="(segment, index) in mentionSegments(entry.text, skillNames)" :key="index">
                        <span
                          v-if="segment.skill"
                          class="rounded bg-accent/12 px-0.5 font-medium text-accent"
                          :title="`Skill ${segment.skill}`"
                          v-text="segment.text"
                        /><span v-else v-text="segment.text" />
                      </template>
                    </p>
                    <p class="mt-1.5! mb-0! flex justify-end gap-2 text-2xs text-muted">
                      <span v-if="entry.delivery" role="status">{{ entry.delivery }}</span>
                      <time :datetime="messageTimes.format(entry.time).datetime" :title="messageTimes.format(entry.time).title">{{ messageTimes.format(entry.time).label }}</time>
                    </p>
                  </div>
                  <p v-else class="m-0! flex justify-end gap-2 text-2xs text-muted">
                    <span v-if="entry.delivery" role="status">{{ entry.delivery }}</span>
                    <time :datetime="messageTimes.format(entry.time).datetime" :title="messageTimes.format(entry.time).title">{{ messageTimes.format(entry.time).label }}</time>
                  </p>
                </article>
                <article v-else-if="entry.kind === 'message'" class="activity-message chat-message my-3 py-2" :class="visibleOutcome && entry.id === outcomeEntryId ? 'mb-1!' : ''">
                  <p class="mb-1.5! flex justify-between gap-3 text-2xs text-muted">
                    <span class="truncate">{{ agent }}<template v-if="entry.via"> · via {{ entry.via }}</template></span>
                    <time :datetime="messageTimes.format(entry.time).datetime" :title="messageTimes.format(entry.time).title">{{ messageTimes.format(entry.time).label }}</time>
                  </p>
                  <ActivityContent :content="entry.text" />
                </article>
                <ChatNotice v-else-if="entry.kind === 'notice'" :notice="entry.artifact" />
                <SubagentTeam v-else-if="entry.kind === 'team'" :agents="entry.agents" :active="active" />
                <PlanCard v-else-if="entry.kind === 'plan'" :plan="entry.plan" :live="entry.id === livePlan && !!plan" />
                <PlanUpdate v-else-if="entry.kind === 'plan-update'" :change="entry.change" :progress="entry.progress" />
                <ChatWaitCard
                  v-else-if="entry.kind === 'wait'"
                  :wait="entry.wait"
                  :live="wakeup"
                  @pause="emit('pauseWait')"
                />
                <AgentActions
                  v-else
                  :artifacts="entry.artifacts"
                  :active="active"
                  :suspended="active && suspended"
                />
                <ChatOutcome
                  v-if="visibleOutcome && entry.id === outcomeEntryId"
                  :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`"
                  :outcome="visibleOutcome"
                  :agent="agent"
                >
                  <slot name="outcome" />
                </ChatOutcome>
              </template>

              <ChatQuotaCard
                v-if="chat && quota"
                :quota="quota"
                :can-manage="!!canManageAccounts"
                :busy="busy"
                @resume="emit('resumeQuota')"
              />

              <ChatOutcome
                v-if="visibleOutcome && !outcomeEntryId"
                :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`"
                :outcome="visibleOutcome"
                :agent="agent"
              >
                <slot name="outcome" />
              </ChatOutcome>

              <p v-if="active && suspended" role="status" class="mt-4 mb-0.5 text-xs text-muted">
                {{ exclusiveSuspensionLabel }}. Work resumes automatically when the reservation and other pauses are cleared.
              </p>
              <div v-else-if="active && !sending?.length && events.length && !(subagentBar && working.subagents)" class="activity-working mt-4 mb-0.5">
                <WorkingIndicator :step="working" />
              </div>
              <div v-else-if="active && !sending?.length && !events.length" class="activity-working flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
                <span class="activity-presence w-[7px] h-[7px] rounded-full bg-[light-dark(#8e8baa,_var(--dark-accent-surface))] shrink-0 live" />Waiting for the worker…
              </div>
              <div v-else-if="!chat && !events.length" class="activity-end flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
                <span />No activity recorded yet<span />
              </div>
            </div>
          </div>
          <!-- An overlay keeps the reading position and the composer still while following is paused. -->
          <Transition
            enter-active-class="transition-opacity duration-180 delay-120"
            leave-active-class="transition-opacity duration-120"
            enter-from-class="opacity-0"
            leave-to-class="opacity-0"
          >
            <button
              v-if="showLatest"
              class="absolute bottom-2 left-1/2 z-10 grid size-9 -translate-x-1/2 place-items-center rounded-full border border-line bg-raised text-ink shadow-sm transition-opacity hover:bg-hover focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent phone:size-11"
              :class="scrolling && !latestFocused ? 'pointer-events-none opacity-0 duration-120' : 'duration-180 delay-120'"
              :aria-label="chat ? 'Latest messages' : 'Latest activity'"
              :title="chat ? 'Latest messages' : 'Latest activity'"
              @click="followLatest"
              @focus="latestFocused = true"
              @blur="latestFocused = false"
            >
              <Icon :name="ArrowDown" :size="18" />
            </button>
          </Transition>
        </div>
      </section>
    </Teleport>
  </div>
  <ArtifactViewer
    v-if="artifactViewer !== null"
    :items="deliverables ?? []"
    :initial="artifactViewer"
    @close="artifactViewer = null"
  />
</template>
