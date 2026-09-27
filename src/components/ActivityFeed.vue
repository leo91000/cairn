<script setup lang="ts">
import type { Deliverable } from '../../shared/artifacts'
import type { RunEvent, TaskOutcome } from '../../shared/contracts'
import type { ActivityArtifact, ActivityEntry } from '../activity'
import type { SendingMessage } from '../chat-delivery'
import type { ReadingPosition } from '../history-cache'
import { twMerge } from 'tailwind-merge'
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { latestArtifacts } from '../../shared/artifacts'
import { activityEntries } from '../activity'
import { deliveryEntries } from '../deliverables'
import { ArrowDown, ChevronDown, Layers, LoaderCircle, Maximize2, Minimize2 } from '../icons'
import { workingStep } from '../signal'
import { mentionSegments } from '../skill-mentions'
import { iconButton } from '../ui'
import ActivityArtifactCard from './ActivityArtifactCard.vue'
import ActivityContent from './ActivityContent.vue'
import AgentActions from './AgentActions.vue'
import AgentAvatar from './AgentAvatar.vue'
import ArtifactGallery from './ArtifactGallery.vue'
import ArtifactRail from './ArtifactRail.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import ChatAttachments from './ChatAttachments.vue'
import ChatNotice from './ChatNotice.vue'
import ChatOutcome from './ChatOutcome.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import WorkingIndicator from './WorkingIndicator.vue'

const props = defineProps<{ events: RunEvent[], active: boolean, agent: string, agentId?: string, task: string, more: boolean, loading: boolean, trimmed: number, preview?: boolean, compactToolbar?: boolean, chat?: boolean, outcome?: TaskOutcome | null, deliverables?: Deliverable[], sending?: SendingMessage[], cacheKey?: string, position?: ReadingPosition, loadingOlder?: boolean, olderError?: string, skills?: string[] }>()
const emit = defineEmits<{ load: [], position: [value: ReadingPosition, key?: string] }>()
const skillNames = computed(() => new Set(props.skills ?? []))
const entries = computed(() => {
  const entries = activityEntries(props.events, props.chat)
  const acknowledged = new Set(props.events.filter(event => event.type === 'chat.user').map(event => event.payload?.messageId))
  for (const { message, label } of props.sending ?? []) {
    if (!acknowledged.has(message.id))
      entries.push({ kind: 'message', role: 'user', id: `sending:${message.id}`, time: message.createdAt, text: message.text, attachments: message.attachments, delivery: label })
  }
  return deliveryEntries(entries, props.deliverables ?? [])
})
const working = computed(() => workingStep(entries.value.filter((entry): entry is ActivityEntry => entry.kind !== 'deliverables'), props.agent, props.events[0]?.createdAt ?? null))
const visibleOutcome = computed(() => props.chat && !props.active && !props.sending?.length && !props.loading ? props.outcome : null)
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
const opened = ref(new Set<string>())
const expanded = ref(new Set<string>())
let previewed = false
let resizeObserver: ResizeObserver | undefined
watch(scroller, (element) => {
  resizeObserver?.disconnect()
  if (!element)
    return
  follow.value = props.position?.follow ?? true
  if (props.position && !follow.value)
    element.scrollTop = props.position.top
  resizeObserver = new ResizeObserver(() => {
    if (follow.value)
      jump()
  })
  resizeObserver.observe(element)
})
onBeforeUnmount(() => {
  savePosition()
  resizeObserver?.disconnect()
})
watch(entries, (items) => {
  if (!props.preview || previewed)
    return
  const group = items.findLast(item => item.kind === 'group' && item.artifacts.some(artifact => artifact.kind !== 'notice'))
  if (!group || group.kind !== 'group')
    return
  opened.value.add(group.id)
  previewed = true
}, { immediate: true })
function toggle(set: Set<string>, id: string) {
  if (set.has(id))
    set.delete(id)
  else
    set.add(id)
}
function groupLabel(artifacts: ActivityArtifact[]) {
  const actions = artifacts.filter(item => item.kind !== 'notice')
  if (!actions.length)
    return artifacts.some(item => item.status === 'error') ? 'Session updates · needs attention' : 'Session updates'
  const kinds = [...new Set(actions.map(item => ({ command: 'terminal', read: 'reading', browse: 'workspace', output: 'output', files: 'files', search: 'research', tool: 'tools', plan: 'plan', thinking: 'thinking', notice: 'updates' })[item.kind]))]
  return kinds.map(value => value[0].toUpperCase() + value.slice(1)).join(' · ')
}
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
let prependAnchor: { height: number, top: number } | undefined
function loadOlder() {
  if (!props.more || props.loadingOlder)
    return
  follow.value = false
  const el = scroller.value
  if (el)
    prependAnchor = { height: el.scrollHeight, top: el.scrollTop }
  emit('load')
}
watch(() => props.loadingOlder, async (loading) => {
  if (loading)
    return
  await nextTick()
  const el = scroller.value
  if (el && prependAnchor && !props.olderError)
    el.scrollTop = prependAnchor.top + el.scrollHeight - prependAnchor.height
  prependAnchor = undefined
})
function scrolled() {
  const el = scroller.value
  if (el && el.scrollHeight - el.scrollTop - el.clientHeight > 60)
    follow.value = false
  if (el && el.scrollTop < 100 && !follow.value && !props.olderError)
    loadOlder()
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
      <dialog ref="viewer" class="activity-fullscreen fixed [inset:0] w-full max-w-none h-dvh max-h-none border-0 bg-raised text-ink p-0 m-0" aria-label="Fullscreen activity" @cancel.prevent="exitFullscreen">
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
            <button v-if="chat" :class="iconButton" aria-label="Follow output" :aria-pressed="follow" :title="follow ? 'Pause auto-scroll' : 'Follow latest output'" @click="follow = !follow; followChanged()">
              <Icon :name="ArrowDown" :size="17" />
            </button>
            <label v-else class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="follow" type="checkbox" @change="followChanged">Follow output</label>
            <button ref="fullscreenButton" :class="twMerge(iconButton, 'icon-button')" :aria-label="fullscreen ? 'Exit fullscreen' : 'Open activity fullscreen'" @click="fullscreen ? exitFullscreen() : enterFullscreen()">
              <Icon v-if="fullscreen" :name="Minimize2" :size="19" /><Icon v-else :name="Maximize2" :size="19" />
            </button>
          </div>
        </header>
        <p v-if="olderError" class="px-5 text-sm text-danger">
          {{ olderError }}
        </p>
        <div ref="scroller" :class="chat ? 'flex flex-col' : ''" class="activity-scroll flex-1 min-h-0 overflow-auto overscroll-contain [scrollbar-width:thin] [scrollbar-color:var(--color-control)_transparent] focus-visible:outline-2 focus-visible:outline-offset-[-3px] focus-visible:outline-accent" tabindex="0" role="region" aria-label="Activity output" @scroll="scrolled">
          <UiButton v-if="more" class="activity-load my-3 flex text-xs mx-auto" :disabled="loadingOlder" @click="loadOlder">
            {{ loadingOlder ? 'Loading history…' : 'Earlier messages' }}
          </UiButton>
          <!-- As on Android, a short conversation sits just above the composer. -->
          <div :class="chat ? 'mt-auto w-full' : ''" class="activity-conversation max-w-205 pt-5 pb-7 px-9 mx-auto my-0 phone:px-4 phone:py-6">
            <div v-if="!chat" class="activity-intro flex items-center gap-3 mb-7 phone:gap-2.5">
              <AgentAvatar :name="agent" :identity="agentId" :size="39" /><div><strong>{{ agent }}</strong></div>
            </div>
            <p v-if="trimmed" class="activity-retention text-2xs text-muted leading-[1.8]">
              Showing the latest {{ events.length.toLocaleString() }} events. {{ trimmed.toLocaleString() }} earlier events are outside this view.
            </p>
            <template v-for="entry in entries" :key="entry.id">
              <div v-if="entry.kind === 'deliverables'" :class="chat ? 'my-3' : 'my-5'">
                <ArtifactRail v-if="chat" :items="entry.files" @open="artifactViewer = $event.id" />
                <ArtifactGallery v-else :items="entry.files" @open="artifactViewer = $event.id" />
              </div>
              <!-- Chat messages follow the Android reader: a tonal bubble for you, plain text for the agent. -->
              <article v-else-if="entry.kind === 'message' && chat && entry.role === 'user'" class="activity-message chat-message ml-auto! my-3 w-fit max-w-[90%] rounded-2xl rounded-br-sm bg-variant px-4 py-3">
                <span class="sr-only">You</span>
                <p class="whitespace-pre-wrap text-lg leading-normal">
                  <template v-for="(segment, index) in mentionSegments(entry.text, skillNames)" :key="index">
                    <span v-if="segment.skill" class="rounded bg-accent/12 px-0.5 font-medium text-accent" :title="`Skill ${segment.skill}`" v-text="segment.text" /><span v-else v-text="segment.text" />
                  </template>
                </p>
                <ChatAttachments v-if="entry.attachments?.length" :attachments="entry.attachments" class="mt-3!" />
                <p class="mt-1.5! mb-0! flex justify-end gap-2 text-2xs text-muted">
                  <span v-if="entry.delivery" role="status">{{ entry.delivery }}</span>
                  <time :datetime="new Date(entry.time).toISOString()" :title="new Date(entry.time).toLocaleString()">{{ new Date(entry.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time>
                </p>
              </article>
              <article v-else-if="entry.kind === 'message' && chat" class="activity-message chat-message my-3 py-2" :class="visibleOutcome && entry.id === outcomeEntryId ? 'mb-1!' : ''">
                <p class="mb-1.5! flex justify-between gap-3 text-2xs text-muted">
                  <span class="truncate">{{ agent }}</span>
                  <time :datetime="new Date(entry.time).toISOString()" :title="new Date(entry.time).toLocaleString()">{{ new Date(entry.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time>
                </p>
                <ActivityContent :content="entry.text" />
              </article>
              <article v-else-if="entry.kind === 'message'" class="activity-message mt-6.5 mb-7.5 mx-0" :class="[entry.role === 'user' ? 'ml-auto! max-w-[85%] rounded-2xl rounded-br-md bg-hover px-5 py-3' : '', visibleOutcome && entry.id === outcomeEntryId ? 'mb-1!' : '']">
                <header><span class="message-dot w-[5px] h-[5px] bg-[light-dark(#4f4c73,_var(--dark-accent-surface))] rounded-full" /><strong>{{ entry.role === 'user' ? 'You' : agent }}</strong><time :datetime="new Date(entry.time).toISOString()" :title="new Date(entry.time).toLocaleString()">{{ new Date(entry.time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time></header>
                <p v-if="entry.role === 'user'" class="whitespace-pre-wrap text-sm leading-relaxed">
                  <template v-for="(segment, index) in mentionSegments(entry.text, skillNames)" :key="index">
                    <span v-if="segment.skill" class="rounded bg-accent/12 px-0.5 font-medium text-accent" :title="`Skill ${segment.skill}`" v-text="segment.text" /><span v-else v-text="segment.text" />
                  </template>
                </p>
                <ActivityContent v-else :content="entry.text" />
                <p v-if="entry.delivery" role="status" class="mt-2! mb-0! text-[11px] text-muted">
                  {{ entry.delivery }}
                </p>
                <ChatAttachments v-if="entry.attachments?.length" :attachments="entry.attachments" class="mt-3!" />
              </article>
              <ChatNotice v-else-if="entry.kind === 'notice'" :notice="entry.artifact" />
              <AgentActions v-else-if="chat" :artifacts="entry.artifacts" :active="active" />
              <section v-else class="activity-group border-line/70 border rounded-xl bg-transparent overflow-hidden mx-0 my-4.5" :class="{ 'expanded': opened.has(entry.id), 'notice-only': entry.artifacts.every(item => item.kind === 'notice') }">
                <button class="activity-group-toggle bg-transparent flex items-center gap-[11px] w-full text-left border-0 text-ink cursor-pointer phone:gap-[9px] px-[17px] py-[15px] phone:px-3 phone:py-[13px]" :aria-expanded="opened.has(entry.id)" :aria-controls="`activity-${entry.id}`" @click="toggle(opened, entry.id)">
                  <span class="activity-group-icon w-8 h-8 grid place-items-center bg-transparent rounded-lg shrink-0"><Icon v-if="active && entry.artifacts.some(item => item.status === 'running')" :name="LoaderCircle" class="activity-spinning [animation:activity-spin_1.5s_linear_infinite] [@media(prefers-reduced-motion:_reduce)]:[animation:none]" :size="17" /><Icon v-else :name="Layers" :size="17" /></span>
                  <span class="activity-group-label min-w-0 flex-1"><strong>{{ groupLabel(entry.artifacts) }}</strong><small>{{ entry.artifacts.length }} {{ entry.artifacts.length === 1 ? 'step' : 'steps' }}<span v-if="entry.artifacts.some(item => item.status === 'error')" class="activity-attention text-danger"> · Includes errors</span></small></span>
                  <Icon :name="ChevronDown" class="activity-chevron [transition:transform_.18s] shrink-0" :size="17" />
                </button>
                <div v-if="opened.has(entry.id)" :id="`activity-${entry.id}`" class="activity-artifacts bg-transparent border-t border-line px-4 py-0 phone:px-3 phone:py-0">
                  <ActivityArtifactCard v-for="artifact in entry.artifacts" :key="artifact.id" :artifact="artifact" :active="active" :expanded="expanded.has(artifact.id)" @toggle="toggle(expanded, artifact.id)" />
                </div>
              </section>
              <ChatOutcome v-if="visibleOutcome && entry.id === outcomeEntryId" :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`" :outcome="visibleOutcome" :agent="agent" />
            </template>

            <ChatOutcome v-if="visibleOutcome && !outcomeEntryId" :key="`${visibleOutcome.messageId}:${visibleOutcome.reportedAt}`" :outcome="visibleOutcome" :agent="agent" />

            <div v-if="active && !sending?.length && events.length" class="activity-working mb-0.5" :class="chat ? 'mt-4' : 'mt-7.5 rounded-2xl border border-line bg-surface px-4 py-3'">
              <WorkingIndicator :step="working" />
            </div>
            <div v-else-if="active && !sending?.length" class="activity-working flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
              <span class="activity-presence w-[7px] h-[7px] rounded-full bg-[light-dark(#8e8baa,_var(--dark-accent-surface))] shrink-0 live" />Waiting for the worker…
            </div>
            <div v-else-if="!chat" class="activity-end flex items-center justify-center gap-3 text-muted text-3xs mt-7.5 mb-0.5 mx-0">
              <span />{{ events.length ? 'End of activity' : 'No activity recorded yet' }}<span />
            </div>
          </div>
        </div>
      </section>
    </Teleport>
  </div>
  <ArtifactViewer v-if="artifactViewer !== null" :items="deliverables ?? []" :initial="artifactViewer" @close="artifactViewer = null" />
</template>
