<script setup lang="ts">
import type { ExecutionNode, NodeStoragePolicy } from '../../../../packages/contracts/nodes'
import { ref } from 'vue'
import { updateNodeStorage } from '../../../../packages/contracts/generated/updateNodeStorage'
import { api } from '../api'
import { HardDrive } from '../icons'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ node: ExecutionNode }>()
const emit = defineEmits<{ saved: [] }>()
const policy = ref<NodeStoragePolicy | null>(null)
const revision = ref('')
const error = ref('')
const busy = ref(false)

function open() {
  error.value = ''
  revision.value = props.node.revision
  const saved = props.node.storage
  policy.value = {
    memoryCacheMiB: saved?.memoryCacheMiB ?? 256,
    reserveMiB: saved?.reserveMiB ?? 10240,
    reservePercent: saved?.reservePercent ?? 5,
  }
}

async function save() {
  if (!policy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await updateNodeStorage(api, { nodeId: props.node.id, revision: revision.value, storage: policy.value })
    policy.value = null
    emit('saved')
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <UiButton variant="default" @click="open">
    <Icon :name="HardDrive" :size="15" /> Edit storage settings
  </UiButton>
  <Modal
    v-if="policy"
    sheet
    :title="`Storage · ${node.name}`"
    @close="policy = null"
  >
    <form class="storage-form" @submit.prevent="save">
      <p>Working files remain local to this node.</p>
      <fieldset>
        <legend>Local disk</legend><label>Shared memory cache (MiB)<input
          v-model.number="policy.memoryCacheMiB"
          type="number"
          min="4"
          max="4096"
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
        <p>The larger free-space reserve applies. Working files stay on this node; executions wait when there is no room to grow.</p>
      </fieldset><p v-if="error" role="alert" class="text-coral">
        {{ error }}
      </p>
      <UiButton type="submit" :disabled="busy">
        {{ busy ? 'Saving…' : 'Save storage settings' }}
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
