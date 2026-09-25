<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { latestArtifacts } from '../../shared/artifacts'
import { api, date, notify } from '../api'
import {
  ArrowLeft,
  Copy,
  FileText,
  RotateCw,
  Square,
  Terminal,
} from '../icons'
import { useLiveRun } from '../use-live-run'
import ActivityFeed from './ActivityFeed.vue'
import ArtifactGallery from './ArtifactGallery.vue'
import ArtifactViewer from './ArtifactViewer.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import Modal from './Modal.vue'
import Outcome from './Outcome.vue'
import RunDetails from './RunDetails.vue'
import Status from './Status.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

const props = defineProps<{ runId: string }>()
const router = useRouter()
const live = useLiveRun(() => `/runs/${props.runId}/stream`)
const { events, connectionNotice, catchingUp: loading } = live
const run = computed(() => live.snapshot.value?.run ?? undefined)
const deliverables = computed(() => live.snapshot.value?.artifacts ?? [])
const artifactViewer = ref<string | null>(null)
const error = ref('')
// As on Android, a run opens on its conversation.
const tab = ref('events')
const confirm = ref(false)
const confirmCleanup = ref(false)
const active = computed(
  () => run.value && ['running', 'queued'].includes(run.value.status),
)
watch(live.error, (value) => {
  if (value)
    error.value = value
})
watch(() => props.runId, () => {
  tab.value = 'events'
})

async function cancel() {
  try {
    await api(`/runs/${run.value!.id}/cancel`, { method: 'POST' })
    confirm.value = false
    notify('Cancellation requested')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function resume() {
  tab.value = 'events'
  try {
    await api(`/runs/${run.value!.id}/resume`, { method: 'POST' })
    notify('Resuming saved conversation')
  }
  catch (e) { error.value = (e as Error).message }
}

async function retry() {
  try {
    const next = await api(`/runs/${run.value!.id}/retry`, { method: 'POST' })
    router.push(`/runs/${next.id}`)
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

async function cleanup() {
  try {
    await api(`/runs/${run.value!.id}/cleanup`, { method: 'POST' })
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
      <RouterLink to="/runs" class="back-link inline-flex items-center gap-[7px] text-muted text-xs mb-[25px]">
        <Icon :name="ArrowLeft" :size="16" />Back to runs
      </RouterLink>
      <UiButton v-if="run && active" variant="danger-outline" @click="confirm = true">
        <Icon :name="Square" :size="15" />Stop run
      </UiButton>
      <div v-else-if="run" class="flex flex-wrap justify-end gap-2">
        <RouterLink v-if="run.trigger === 'chat'" :to="`/chats/${run.taskId}`" class="text-sm text-accent">
          Open chat
        </RouterLink>
        <UiButton v-else size="small" @click="retry">
          Run again
        </UiButton>
        <UiButton
          v-if="run.resumeAvailable && run.trigger !== 'chat'"
          size="small"
          variant="primary"
          @click="resume"
        >
          <Icon :name="RotateCw" :size="16" />Resume
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
            <Status :status="run.status" /><Outcome :outcome="run.outcome" :status="run.status" /><span v-if="run.accountName">{{ run.accountName }}</span><span>{{ run.snapshot.agent.name }} · {{ date(run.createdAt) }}</span>
          </div>
        </div>
      </div>
      <p v-if="run.accountWaitReason" role="status" class="mb-3 shrink-0 text-sm text-warning">
        {{ run.accountWaitReason }}
      </p>
      <section class="panel run-panel flex flex-1 min-h-0 flex-col overflow-hidden">
        <header class="run-panel-head flex shrink-0 items-center justify-between border-b border-line p-[7px] phone:p-[5px]">
          <UiSegments v-model="tab" label="Run view" :options="[{ value: 'events', label: 'Conversation', icon: Terminal }, { value: 'result', label: 'Result', icon: FileText }, { value: 'files', label: 'Files', count: latestArtifacts(deliverables).length }, { value: 'brief', label: 'Mission brief' }]" />
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
            <span class="pulse-ring w-8 h-8 rounded-full border-2 border-line [border-top-color:light-dark(#6660a5,_var(--dark-border))] animate-spin mb-[17px]" />
            <h3>{{ active ? "Work is underway" : "No summary yet" }}</h3>
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
          :active="!!active"
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
          <RunDetails :run="run" :active="!!active" @cleanup="confirmCleanup = true" />
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
        <p>
          The running process will stop. Files and external changes already made
          will remain, so review them before running the task again.
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
