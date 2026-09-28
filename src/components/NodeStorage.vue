<script setup lang="ts">
import type { ExecutionNode, NodeStoragePolicy } from '../../shared/nodes'
import { ref } from 'vue'
import { api } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ node: ExecutionNode }>()
const emit = defineEmits<{ saved: [] }>()
const policy = ref<NodeStoragePolicy | null>(null)
const error = ref('')
const busy = ref(false)
function open() {
  error.value = ''
  const saved = props.node.storage
  policy.value = {
    cacheMiB: saved?.cacheMiB ?? 102400,
    reserveMiB: saved?.reserveMiB ?? 10240,
    reservePercent: saved?.reservePercent ?? 5,
    backupSeconds: saved?.backupSeconds ?? 60,
    maxDirtySeconds: saved?.maxDirtySeconds ?? 300,
  }
}
async function save() {
  busy.value = true
  error.value = ''
  try {
    await api(`/nodes/${props.node.id}/storage`, { method: 'PUT', body: JSON.stringify(policy.value) })
    policy.value = null
    emit('saved')
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <UiButton variant="default" @click="open">
    Storage
  </UiButton>
  <Modal v-if="policy" :title="`Storage · ${node.name}`" @close="policy = null">
    <form class="grid gap-4" @submit.prevent="save">
      <p>Files are saved to S3 and loaded when needed. Conversations remain active when their local cache is freed.</p>
      <label>Clean cache budget (MiB)<input v-model.number="policy.cacheMiB" type="number" min="0" max="16777216" required></label>
      <label>Minimum free disk (MiB)<input v-model.number="policy.reserveMiB" type="number" min="64" max="16777216" required></label>
      <label>Minimum free disk (%)<input v-model.number="policy.reservePercent" type="number" min="1" max="50" required></label>
      <p>The larger free-space reserve applies. Unsaved work stays local; executions pause before the reserve is exhausted.</p>
      <label>Synchronization target (seconds)<input v-model.number="policy.backupSeconds" type="number" min="5" max="3600" required></label>
      <label>Pause after unsaved changes (seconds)<input v-model.number="policy.maxDirtySeconds" type="number" :min="policy.backupSeconds" max="86400" required></label>
      <p v-if="error" role="alert" class="text-coral">
        {{ error }}
      </p>
      <UiButton type="submit" :disabled="busy">
        {{ busy ? 'Checking storage…' : 'Save storage settings' }}
      </UiButton>
    </form>
  </Modal>
</template>
