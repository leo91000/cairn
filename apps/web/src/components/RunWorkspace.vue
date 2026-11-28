<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { latestArtifacts } from '../../../../packages/contracts/artifacts'
import { cancelRun } from '../../../../packages/contracts/generated/cancelRun'

import { cleanupRun } from '../../../../packages/contracts/generated/cleanupRun'
import { canUseAgent } from '../agent-use'
import {
  api,
  date,
  notify,
  state,
} from '../api'
import { recordedTitle } from '../chat-wait'
import {
  ArrowLeft,
  CalendarClock,
  Copy,
  FileText,
  Hourglass,
  Square,
  Terminal,
} from '../icons'
import { useRetryState } from '../run-retry'
import {
  canStopRun,
  exclusiveSuspensionLabel,
  isExclusivelySuspended,
  runDisplayStatus,
} from '../run-status'
import { useLiveRun } from '../use-live-run'
import ActivityFeed from './ActivityFeed.vue'
import ArtifactGallery from './ArtifactGallery.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import Modal from './Modal.vue'
import Outcome from './Outcome.vue'
import RecoveryNotice from './RecoveryNotice.vue'
import RunDetails from './RunDetails.vue'
import Status from './Status.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiNotice from './UiNotice.vue'
import UiSegments from './UiSegments.vue'

const props = withDefaults(defineProps<{
  runId: string
  back?: { to: string, label: string }
}>(), {
  back: () => ({ to: '/runs', label: 'Back to runs' }),
})
const live = useLiveRun(() => `/runs/${props.runId}/stream`)
const {
  events,
  timeline,
  connectionNotice,
  catchingUp: loading,
} = live
const run = computed(() => live.snapshot.value?.run ?? undefined)
const waiting = computed(() =>
  run.value?.status === 'waiting' || (run.value?.status === 'succeeded' && !!run.value.wakeup),
)
const pausedWait = computed(() => live.snapshot.value?.runConversationPaused === true)
const suspended = computed(() => isExclusivelySuspended(run.value))
const recovery = useRetryState(() => run.value)
// What the run waits for, above its tabs: its scheduled continuation, a retry or an account.
const waitNotice = computed(() => {
  const wakeup = run.value?.wakeup
  if (waiting.value && wakeup) {
    return {
      title: pausedWait.value ? 'Scheduled continuation paused' : recordedTitle(wakeup),
      detail: wakeup.reason,
    }
  }

  if (recovery.value)
    return undefined
  const reason = run.value?.accountWaitReason
  return reason ? { title: 'Waiting', detail: reason } : undefined
})
const deliverables = computed(() => live.snapshot.value?.artifacts ?? [])
const artifactViewer = ref<string | null>(null)
const error = ref('')
// As on Android, a run opens on its conversation.
const tab = ref('events')
const confirm = ref(false)
const confirmCleanup = ref(false)
const active = computed(
  () => run.value && (waiting.value || ['running', 'queued'].includes(run.value.status)),
)
// A delegated conversation can be readable through its requester, but cleanup needs
// access to the run's own agent.
const canCleanUpWorktree = computed(() =>
  !!run.value && !live.snapshot.value?.workspaceCleanupBlockedByInput
  && canUseAgent(state.usableAgents, run.value.snapshot?.agent?.id),
)
const tabs = computed(() => [
  { value: 'events', label: 'Conversation', icon: Terminal },
  { value: 'result', label: 'Result', icon: FileText },
  { value: 'files', label: 'Files', count: latestArtifacts(deliverables.value).length },
  { value: 'brief', label: 'Run details', icon: CalendarClock },
])
watch(live.error, (value) => {
  if (value)
    error.value = value
})
watch(() => props.runId, () => {
  tab.value = 'events'
})

async function cancel() {
  try {
    await cancelRun(api, { runId: run.value!.id })
    confirm.value = false
    notify('Cancellation requested')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function cleanup() {
  try {
    await cleanupRun(api, { runId: run.value!.id })
    confirmCleanup.value = false
    notify('Worktree removed. Its Git branch is preserved.')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function copy() {
  try {
    await navigator.clipboard.writeText(run.value?.summary ?? '')
    notify('Result copied')
  }
  catch {
    error.value = 'Clipboard unavailable. Select and copy the result manually.'
  }
}
</script>

<template>
  <div class="run-workspace flex flex-col flex-1 min-w-0 min-h-0">
    <div class="run-navigation flex items-center justify-between gap-3 mb-4 phone:mb-2.5">
      <RouterLink :to="back.to" class="back-link inline-flex items-center gap-[7px] text-muted text-xs mb-[25px]">
        <Icon :name="ArrowLeft" :size="16" />{{ back.label }}
      </RouterLink>
      <div v-if="run" class="flex flex-wrap items-center justify-end gap-2">
        <RouterLink :to="`/chats/${run.taskId}`" class="text-sm text-accent">
          Open chat
        </RouterLink>
        <UiButton v-if="canStopRun(run) || waiting" variant="danger-outline" @click="confirm = true">
          <Icon :name="Square" :size="15" />Stop run
        </UiButton>
      </div>
    </div>
    <p v-if="!run && connectionNotice" role="status" class="px-4 py-2 text-xs text-muted">
      {{ connectionNotice }}
    </p>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <template v-if="run">
      <div class="page-heading flex items-center justify-between gap-5 mb-[27px] phone:gap-2.5 phone:flex-wrap phone:mb-[21px]">
        <div>
          <h1 :title="run.snapshot.task.name">
            {{ run.snapshot.task.name }}
          </h1>
          <div class="run-title-meta flex items-center gap-3 mt-3.5 text-subtle text-xs phone:flex-wrap phone:text-xs">
            <Status :status="runDisplayStatus(run)" /><Outcome :outcome="run.outcome" :status="run.status" /><span v-if="run.accountName">{{ run.accountName }}</span><span>{{ run.snapshot.agent.name }} · {{ date(run.createdAt) }}</span>
          </div>
        </div>
      </div>
      <RecoveryNotice
        v-if="recovery"
        :recovery="recovery"
        :busy="!!run.cancelRequestedAt"
        @cancel="cancel"
      />
      <UiNotice
        v-else-if="waitNotice"
        role="status"
        class="mt-0 mb-3 shrink-0"
        :icon="Hourglass"
        :title="waitNotice.title"
      >
        <p class="line-clamp-2" :title="waitNotice.detail">
          {{ waitNotice.detail }}
        </p>
      </UiNotice>
      <section class="panel run-panel flex flex-1 min-h-0 flex-col overflow-hidden">
        <header class="run-panel-head flex shrink-0 items-center justify-between border-b border-line p-[7px] phone:p-[5px]">
          <UiSegments v-model="tab" label="Run view" :options="tabs" />
        </header>
        <div v-if="tab === 'result' && run.summary" class="run-panel-actions flex items-center justify-end border-b border-line px-5 py-2 phone:px-4 phone:py-1">
          <UiButton
            v-if="tab === 'result' && run.summary"
            size="small"
            aria-label="Copy result"
            @click="copy"
          >
            <Icon :name="Copy" :size="16" />Copy result
          </UiButton>
        </div>
        <div v-if="tab === 'result'" class="result-content flex-1 min-h-0 overflow-auto overscroll-contain [scrollbar-width:thin] text-sm leading-[1.8] p-7.5 phone:p-5.5">
          <ArtifactGallery
            v-if="deliverables.length"
            class="mb-6"
            :items="deliverables.filter(item => !deliverables.some(other => other.key === item.key && other.version > item.version))"
            @open="artifactViewer = $event.id"
          />
          <Markdown v-if="run.summary" :content="run.summary" />
          <div v-else class="mini-empty flex flex-col items-center text-center pt-7 pb-8.5 text-subtle px-6">
            <span v-if="!suspended && !waiting && active" class="pulse-ring w-8 h-8 rounded-full border-2 border-line [border-top-color:light-dark(#6660a5,_var(--dark-border))] animate-spin mb-[17px]" />
            <h3>{{ pausedWait ? 'Scheduled continuation paused' : waiting ? 'Waiting for scheduled continuation' : suspended ? exclusiveSuspensionLabel : active ? "Work is underway" : "No summary yet" }}</h3>
            <p>
              Open Conversation to follow this run.
            </p>
          </div>
        </div>
        <ActivityFeed
          v-else-if="tab === 'events'"
          :key="run.id"
          :cache-key="`/runs/${run.id}/stream`"
          :position="live.position.value"
          :deliverables="deliverables"
          :events="events"
          :timeline="timeline"
          :active="!!active && !waiting"
          :wakeup="{ wakeup: run.wakeup ?? null, paused: pausedWait, settled: waiting, canPause: false }"
          :suspended="suspended"
          :agent-id="run.snapshot.agent.id"
          :agent="run.snapshot.agent.name"
          :task="run.snapshot.task.name"
          :more="live.hasOlder.value"
          :loading-older="live.loadingOlder.value"
          :older-error="live.olderError.value"
          :loading="loading"
          :trimmed="0"
          :outcome="run.status === 'succeeded' ? run.outcome : null"
          @load="live.loadOlder"
          @position="live.savePosition"
        />
        <div v-else-if="tab === 'files'" class="result-content flex-1 min-h-0 overflow-auto p-5">
          <ArtifactGallery v-if="deliverables.length" :items="latestArtifacts(deliverables)" @open="artifactViewer = $event.id" />
          <p v-else class="text-sm text-muted">
            No files published yet.
          </p>
        </div>
        <div v-else class="result-content flex-1 min-h-0 overflow-auto overscroll-contain [scrollbar-width:thin] text-sm leading-[1.8] p-7.5 phone:p-5.5">
          <RunDetails
            :run="run"
            :active="!!active"
            :can-clean-up-worktree="canCleanUpWorktree"
            @cleanup="confirmCleanup = true"
          />
        </div>
        <p v-if="connectionNotice" role="status" class="shrink-0 border-t border-line px-4 py-2 text-xs text-muted">
          {{ connectionNotice }}
        </p>
      </section>
      <p class="muted text-muted run-id text-2xs mt-[17px] wrap-anywhere">
        Run {{ run.id
        }}<span v-if="run.sessionId"> · Codex session {{ run.sessionId }}</span>
      </p>
    </template><Modal v-if="confirm" title="Stop this run?" @close="confirm = false">
      <div class="modal-body px-6.5 py-6 phone:p-5">
        <p v-if="waiting">
          This cancels the scheduled continuation.
        </p>
        <p v-else>
          The conversation will pause. Files and external changes already made
          will remain, so review them before continuing the conversation.
        </p>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton @click="confirm = false">
          Keep running
        </UiButton><UiButton variant="danger" @click="cancel">
          Stop run
        </UiButton>
      </footer>
    </Modal>
    <Modal
      v-if="confirmCleanup"
      title="Clean up this worktree?"
      @close="confirmCleanup = false"
    >
      <div class="modal-body px-6.5 py-6 phone:p-5">
        <p>
          This removes the saved worktree directory. Its Git branch and run
          history remain available. Worktrees containing changes or untracked
          files cannot be removed.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton @click="confirmCleanup = false">
          Keep worktree
        </UiButton><UiButton variant="danger" @click="cleanup">
          Clean up worktree
        </UiButton>
      </footer>
    </Modal>
  </div>
  <ArtifactViewer
    v-if="artifactViewer !== null"
    :items="deliverables"
    :initial="artifactViewer"
    @close="artifactViewer = null"
  />
</template>
