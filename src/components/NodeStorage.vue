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
  policy.value = { enabled: false, cacheMiB: 102400, reserveMiB: 10240, reservePercent: 5, backupSeconds: 60, maxDirtySeconds: 300, automaticArchiving: false, ...props.node.storage }
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
      <p>Keep files on S3 and load them when needed. Conversations remain active when their local cache is freed. Existing stopped environments migrate one at a time.</p>
      <label><input v-model="policy.enabled" type="checkbox"> Enable storage on demand</label>
      <label>Clean cache budget (MiB)<input v-model.number="policy.cacheMiB" type="number" min="0" max="16777216" required></label>
      <label>Minimum free disk (MiB)<input v-model.number="policy.reserveMiB" type="number" min="64" max="16777216" required></label>
      <label>Minimum free disk (%)<input v-model.number="policy.reservePercent" type="number" min="1" max="50" required></label>
      <p>The larger free-space reserve applies. Unsaved work stays local; executions pause before the reserve is exhausted.</p>
      <label>Backup target (seconds)<input v-model.number="policy.backupSeconds" type="number" min="5" max="3600" required></label>
      <label>Pause after unsaved changes (seconds)<input v-model.number="policy.maxDirtySeconds" type="number" :min="policy.backupSeconds" max="86400" required></label>
      <label><input v-model="policy.automaticArchiving" type="checkbox"> Also apply automatic conversation archiving</label>
      <p v-if="node.storageMigration?.error" role="status">
        Migration: {{ node.storageMigration.error }}
      </p>
      <p v-if="error" role="alert" class="text-coral">
        {{ error }}
      </p>
      <UiButton type="submit" :disabled="busy">
        {{ busy ? 'Checking storage…' : 'Save storage settings' }}
      </UiButton>
    </form>
  </Modal>
</template>
