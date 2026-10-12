<script setup lang="ts">
import type { RunListItem, Task } from '../../../../packages/contracts/contracts'
import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { useMissionActions } from '../mission-actions'
import { useMissionRuns } from '../mission-runs'
import { filRunPath } from '../signal'
import MissionDetail from './MissionDetail.vue'
import Modal from './Modal.vue'
import TaskEditor from './TaskEditor.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// The mission behind a run opened from the Fil: the same detail as in Missions, with its runs
// opening beside the Fil.
const props = defineProps<{ task: Task }>()
const router = useRouter()
const runs = useMissionRuns()
const latest = computed(() => runs.value.reduce<RunListItem | undefined>((newest, item) => item.taskId === props.task.id && item.trigger !== 'chat' && (!newest || item.createdAt > newest.createdAt) ? item : newest, undefined))
const editor = ref<Task | null>(null)
const {
  busy,
  error,
  deleting,
  run,
  pause,
  archive,
  duplicate,
  remove,
} = useMissionActions({
  started: queued => router.push(filRunPath(queued.id)),
  removed: () => router.push('/'),
})
</script>

<template>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <MissionDetail
    :task="task"
    :latest="latest"
    :busy="busy === task.id"
    :run-path="filRunPath"
    @run="run(task)"
    @edit="editor = task"
    @pause="pause(task)"
    @duplicate="duplicate(task)"
    @archive="archive(task)"
    @remove="deleting = task"
  />
  <TaskEditor
    v-if="editor"
    :task="editor"
    @saved="editor = null"
    @close="editor = null"
  />
  <Modal v-if="deleting" title="Delete this mission?" @close="deleting = null">
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <p>
        “{{ deleting.name }}” will stop scheduling. Its existing run history
        will remain available.
      </p>
    </div>
    <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
      <UiButton @click="deleting = null">
        Keep mission
      </UiButton><UiButton variant="danger" @click="remove">
        Delete mission
      </UiButton>
    </footer>
  </Modal>
</template>
