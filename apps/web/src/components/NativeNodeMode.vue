<script setup lang="ts">
import type { ExecutionNode, NativeModePreview } from '../../../../packages/contracts/nodes'
import { computed, ref } from 'vue'
import { nativeModeLabel } from '../../../../packages/contracts/nodes'
import { api } from '../api'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ node: ExecutionNode }>()
const emit = defineEmits<{ changed: [] }>()
const preview = ref<NativeModePreview | null>(null)
const busy = ref(false)
const error = ref('')
const target = computed(() => props.node.nativeModeProposal?.mode ?? (['pending', 'failed'].includes(props.node.nativeModeStatus ?? '') ? props.node.nativeModeRequest?.mode ?? 'protected' : props.node.nativeModeRequest?.mode === 'administrator' ? 'protected' : 'administrator'))
const confirmed = computed(() => {
  const transitioning = ['pending', 'failed'].includes(props.node.nativeModeStatus ?? '')
  const legacyProtected = !props.node.nativeModeObserved && !props.node.nativeModeRequest
  return !transitioning && (props.node.nativeModeObserved?.status === 'applied' || legacyProtected)
})
const effective = computed(() => {
  if (props.node.nativeModeObserved) {
    const label = nativeModeLabel(props.node.nativeModeObserved.mode)
    return confirmed.value ? label : `Last confirmed: ${label}`
  }

  return props.node.nativeModeRequest ? 'Effective mode unconfirmed' : 'Native protected'
})

async function open(mode = target.value) {
  busy.value = true
  error.value = ''
  try {
    preview.value = await api(`/nodes/${props.node.id}/native-mode`, { method: 'POST', body: JSON.stringify({ mode }) })
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function confirm() {
  if (!preview.value)
    return
  busy.value = true
  error.value = ''
  try {
    await api(`/nodes/${props.node.id}/native-mode`, { method: 'POST', body: JSON.stringify({ mode: preview.value.preview.mode, previewId: preview.value.previewId, confirm: true }) })
    preview.value = null
    emit('changed')
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="my-3 grid gap-2 rounded-xl border border-line p-4 text-sm" aria-label="Native host privileges">
    <p><strong>{{ effective }}</strong><span v-if="confirmed"> · effective on this node</span></p>
    <p v-if="!confirmed">
      Current privileges are unconfirmed; new placements are blocked.
    </p>
    <p v-if="node.nativeModeStatus === 'pending'">
      Change in progress. New placements wait for stop and mode confirmation.
    </p>
    <UiAlert v-if="node.nativeModeStatus === 'failed' || node.nativeModeObserved?.status === 'failed'">
      Change failed; new placements are blocked. {{ node.nativeModeObserved?.error }} Retry after checking the supervisor.
    </UiAlert>
    <p v-if="node.nativeModeProposal">
      An agent requested {{ nativeModeLabel(node.nativeModeProposal.mode) }}. Owner confirmation required.
    </p>
    <p v-if="node.capabilities.nativeModeProtocol !== 1" class="text-muted">
      Update the supervisor and install sudo with its standard sudoers.d policy to change privileges.
    </p>
    <UiButton v-else :disabled="busy || node.revoked || node.nativeModeStatus === 'pending'" @click="open()">
      {{ node.nativeModeProposal ? 'Review requested mode' : `Switch to ${nativeModeLabel(target)}` }}
    </UiButton>
    <UiButton v-if="node.nativeModeStatus === 'failed' && node.nativeModeRequest?.mode === 'administrator'" :disabled="busy" @click="open('protected')">
      Return to Native protected
    </UiButton>
    <UiAlert v-if="error && !preview">
      {{ error }}
    </UiAlert>
    <Modal v-if="preview" :title="`Switch ${node.name} to ${nativeModeLabel(preview.preview.mode)}?`" @close="preview = null">
      <div class="grid gap-4">
        <p>{{ preview.preview.warning }}</p>
        <p>{{ preview.preview.interruptions }}</p>
        <p><strong>Trusted people:</strong> {{ (preview.preview.trustedPeople?.map(person => person.email || person.id) ?? preview.preview.trustedAccounts).join(', ') || 'none' }}</p>
        <div>
          <strong>Agents receiving this access:</strong><ul>
            <li v-for="agent in preview.preview.agents" :key="agent.id">
              {{ agent.name }}
            </li>
          </ul>
        </div>
        <div>
          <strong>Affected conversations ({{ preview.preview.conversations.length }}):</strong><ul>
            <li v-for="run in preview.preview.conversations" :key="run.id">
              {{ run.name || run.id }} · {{ run.status }}
            </li>
          </ul>
        </div>
        <div>
          <strong>Scheduled tasks:</strong><ul>
            <li v-for="task in preview.preview.tasks" :key="task.id">
              {{ task.name }}
            </li>
          </ul>
        </div>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton variant="danger" :disabled="busy" @click="confirm">
          Confirm access and restart managed executions
        </UiButton>
      </div>
    </Modal>
  </section>
</template>
