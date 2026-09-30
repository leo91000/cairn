<script setup lang="ts">
import type { ExecutionNode, NodeStoragePolicy } from '../../shared/nodes'
import { ref } from 'vue'
import { api } from '../api'
import { HardDrive } from '../icons'
import Icon from './Icon.vue'
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
    memoryCacheMiB: saved?.memoryCacheMiB ?? 256,
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
    <Icon :name="HardDrive" :size="15" /> Storage
  </UiButton>
  <Modal
    v-if="policy"
    sheet
    :title="`Storage · ${node.name}`"
    @close="policy = null"
  >
    <form class="storage-form" @submit.prevent="save">
      <p>Files are saved to S3 and loaded when needed. Conversations remain active when their local cache is freed.</p>
      <fieldset>
        <legend>Local disk</legend><label>Shared memory cache (MiB)<input
          v-model.number="policy.memoryCacheMiB"
          type="number"
          min="4"
          max="4096"
          required
        ></label><label>Clean disk cache budget (MiB)<input
          v-model.number="policy.cacheMiB"
          type="number"
          min="0"
          max="16777216"
          required
        ></label>
        <label>Minimum free disk (MiB)<input
          v-model.number="policy.reserveMiB"
          type="number"
          min="64"
          max="16777216"
          required
        ></label>
        <label>Minimum free disk (%)<input
          v-model.number="policy.reservePercent"
          type="number"
          min="1"
          max="50"
          required
        ></label>
        <p>The larger free-space reserve applies. Unsaved work stays local; executions pause before the reserve is exhausted.</p>
      </fieldset><fieldset>
        <legend>Synchronization</legend><label>Synchronization target (seconds)<input
          v-model.number="policy.backupSeconds"
          type="number"
          min="5"
          max="3600"
          required
        ></label>
        <label>Maximum active synchronization delay (seconds)<input
          v-model.number="policy.maxDirtySeconds"
          type="number"
          :min="policy.backupSeconds"
          max="86400"
          required
        ></label>
        <p>Old unsaved work is synchronized first on resume. This limit bounds how long the resumed execution can continue without publication.</p>
      </fieldset><p v-if="error" role="alert" class="text-coral">
        {{ error }}
      </p>
      <UiButton type="submit" :disabled="busy">
        {{ busy ? 'Checking storage…' : 'Save storage settings' }}
      </UiButton>
    </form>
  </Modal>
</template>

<style scoped>
.storage-form { padding: 24px; display: grid; gap: 20px; }
.storage-form p { font-size: 12px; color: var(--color-muted); }
.storage-form fieldset { display: grid; gap: 16px; border-top: 1px solid var(--color-line); padding-top: 16px; }
.storage-form legend { font-weight: 600; padding-right: 12px; font-size: 13px; }
.storage-form label { display: grid; gap: 6px; font-size: 12px; color: var(--color-muted); }
.storage-form input { width: 100%; color: var(--color-ink); }
</style>
