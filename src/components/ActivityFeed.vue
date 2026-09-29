<script setup lang="ts">
import type { Deliverable } from '../../shared/artifacts'
import type { RunEvent, TaskOutcome } from '../../shared/contracts'
import type { ActivityEntry } from '../activity'
import type { SendingMessage } from '../chat-delivery'
import type { ReadingPosition } from '../history-cache'
import { twMerge } from 'tailwind-merge'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { latestArtifacts } from '../../shared/artifacts'
import { activityEntries } from '../activity'
import { deliveryEntries } from '../deliverables'
import { ArrowDown, Maximize2, Minimize2 } from '../icons'
import { workingStep } from '../signal'
import { mentionSegments } from '../skill-mentions'
import { iconButton } from '../ui'
import ActivityContent from './ActivityContent.vue'
import AgentActions from './AgentActions.vue'
import ArtifactRail from './ArtifactRail.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import ChatAttachments from './ChatAttachments.vue'
import ChatNotice from './ChatNotice.vue'
import ChatOutcome from './ChatOutcome.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import WorkingIndicator from './WorkingIndicator.vue'

const props = defineProps<{
  events: RunEvent[]
  active: boolean
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
}>()
const emit = defineEmits<{ load: [], position: [value: ReadingPosition, key?: string] }>()
const skillNames = computed(() => new Set(props.skills ?? []))
const entries = computed(() => {
  const entries = activityEntries(props.events, props.chat)
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
const working = computed(() => workingStep(entries.value.filter((entry): entry is ActivityEntry => entry.kind !== 'deliverables'), props.agent, props.events[0]?.createdAt ?? null))
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
let pagingFrame: number | undefined
let prependPending = false
let resizeObserver: ResizeObserver | undefined
watch([scroller, conversation], ([element, content]) => {
  resizeObserver?.disconnect()
  if (!element)
    return
  follow.value = props.position?.follow ?? true
  if (follow.value)
    jump()
  else if (props.position)
    element.scrollTop = props.position.top
  resizeObserver = new ResizeObserver(() => {
    if (follow.value)
      jump()
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
  if (pagingFrame !== undefined)
    cancelAnimationFrame(pagingFrame)
})

function jump() {
  scroller.value?.scrollTo({ top: scroller.value.scrollHeight })
}

function savePosition() {
  if (scroller.value)
    emit('position', { top: scroller.value.scrollTop, follow: follow.value }, props.cacheKey)
}

function followChanged() {
  if (follow.value)
    jump()
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
  // Capture immediately before rendering the page, not when the request starts:
  // the reader may have kept scrolling while the response was in flight.
  const el = scroller.value
  const anchor = el && { height: el.scrollHeight, top: el.scrollTop }
  await nextTick()
  if (el && anchor && !props.olderError) {
    if (follow.value)
      jump()
    else
      el.scrollTop = anchor.top + el.scrollHeight - anchor.height
  }

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
    // Match Android's three-screen buffer, including pages that fold into only
    // a few rows. Following new output is independent from fetching history.
    if (el.scrollTop < 3 * el.clientHeight && !props.olderError)
      loadOlder()
  })
}

watch([entries, () => props.more, () => props.loading, () => props.olderError], schedulePaging, { flush: 'post' })

function scrolled() {
  const el = scroller.value
  if (el && el.scrollHeight - el.scrollTop - el.clientHeight > 60)
    follow.value = false
  schedulePaging()
  savePosition()
}

watch([entries, () => props.outcome?.reportedAt, () => props.loading], async () => {
  if (!follow.value)
    return
  await nextTick()
  jump()
})
let fullscreenReturnFocus: HTMLElement | null = null

async function enterFullscreen() {
  fullscreenReturnFocus = document.activeElement as HTMLElement | null
  viewer.value?.showModal()
  fullscreen.value = true
  await nextTick()
  fullscreenButton.value?.focus()
  if (follow.value)
    jump()
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
  following: follow,
  toggleFollow: () => {
    follow.value = !follow.value
    followChanged()
  },
  openFiles: () => { artifactViewer.value = '' },
  enterFullscreen,
  followLatest: () => {
    follow.value = true
    followChanged()
  },
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
            <span class="activity-presence w-[7px] h-[7px] rounded-full bg-[light-dark(#8e8baa,_var(--dark-accent-surface))] shrink-0" :class="{ live: active }" /><span>{{ fullscreen ? task : chat ? (active ? 'Working' : 'Conversation') : 'Agent activity' }}</span>
          </div>
          <div class="activity-toolbar-controls ml-auto flex items-center gap-5 shrink-0 phone:flex-1 phone:justify-end phone:gap-4">
            <button v-if="deliverables?.length" class="rounded-md px-2 py-2 text-xs text-muted hover:bg-hover hover:text-ink" @click="artifactViewer = ''">
              Files · {{ latestArtifacts(deliverables).length }}
            </button>
            <button
              v-if="chat"
              :class="iconButton"
              aria-label="Follow output"
              :aria-pressed="follow"
              :title="follow ? 'Pause auto-scroll' : 'Follow latest output'"
              @click="follow = !follow; followChanged()"
            >
              <Icon :name="ArrowDown" :size="17" />
            </button>
            <label v-else class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="follow" type="checkbox" @change="followChanged">Follow output</label>
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
          <div v-if="olderError || (loadingOlder && atStart)" role="status" class="absolute top-2 left-1/2 -translate-x-1/2 z-10 flex items-center gap-2 rounded-xl bg-raised px-3 py-2 text-xs shadow-md max-w-[90%]">
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
            class="activity-scroll flex flex-col flex-1 min-h-0 overflow-auto overscroll-contain [overflow-anchor:none] [scrollbar-width:thin] [scrollbar-color:var(--color-control)_transparent] focus-visible:outline-2 focus-visible:outline-offset-[-3px] focus-visible:outline-accent"
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
                <article v-else-if="entry.kind === 'message' && entry.role === 'user'" class="activity-message chat-message ml-auto! my-3 w-fit max-w-[90%] rounded-2xl rounded-br-sm bg-variant px-4 py-3">
                  <span class="sr-only">You</span>
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
                  <ChatAttachments v-if="entry.attachments?.length" :attachments="entry.attachments" class="mt-3!" />
                  <p class="mt-1.5! mb-0! flex justify-end gap-2 text-2xs text-muted">
                    <span v-if="entry.delivery" role="status">{{ entry.delivery }}</span>
                    <time :datetime="new Date(entry.time).toISOString()" :title="new Date(entry.time).toLocaleString()">{{ new Date(entry.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time>
                  </p>
                </article>
                <article v-else-if="entry.kind === 'message'" class="activity-message chat-message my-3 py-2" :class="visibleOutcome && entry.id === outcomeEntryId ? 'mb-1!' : ''">
                  <p class="mb-1.5! flex justify-between gap-3 text-2xs text-muted">
                    <span class="truncate">{{ agent }}</span>
                    <time :datetime="new Date(entry.time).toISOString()" :title="new Date(entry.time).toLocaleString()">{{ new Date(entry.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time>
                  </p>
                  <ActivityContent :content="entry.text" />
                </article>
                <ChatNotice v-else-if="entry.kind === 'notice'" :notice="entry.artifact" />
                <AgentActions v-else :artifacts="entry.artifacts" :active="active" />
                <ChatOutcome
                  v-if="visibleOutcome && entry.id === outcomeEntryId"
                  :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`"
                  :outcome="visibleOutcome"
                  :agent="agent"
                />
              </template>

              <ChatOutcome
                v-if="visibleOutcome && !outcomeEntryId"
                :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`"
                :outcome="visibleOutcome"
                :agent="agent"
              />

              <div v-if="active && !sending?.length && events.length" class="activity-working mt-4 mb-0.5">
                <WorkingIndicator :step="working" />
              </div>
              <div v-else-if="active && !sending?.length" class="activity-working flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
                <span class="activity-presence w-[7px] h-[7px] rounded-full bg-[light-dark(#8e8baa,_var(--dark-accent-surface))] shrink-0 live" />Waiting for the worker…
              </div>
              <div v-else-if="!chat && !events.length" class="activity-end flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
                <span />No activity recorded yet<span />
              </div>
            </div>
          </div>
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
