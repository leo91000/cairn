<script setup lang="ts">
import { computed, onUnmounted, ref } from 'vue'
import { api } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

interface Migration {
  chatId: string
  required: boolean
  optional?: boolean
  fromBackend: 'native' | 'firecracker'
  toBackend: 'native' | 'firecracker'
  nodes: { id: string, name: string }[]
  pending: boolean
  environmentExpired?: boolean
  reason?: string | null
  error?: string | null
}

const props = defineProps<{ runId: string }>()
const emit = defineEmits<{ completed: [] }>()
const state = ref<Migration>()
const destination = ref('')
const confirm = ref(false)
const busy = ref(false)
const error = ref('')
const completed = ref(false)
const label = computed(() => state.value?.toBackend === 'native' ? 'native execution' : 'a Firecracker VM')
const options = computed(() => state.value?.nodes.map(node => ({ value: node.id, label: node.name })) || [])
let stopped = false
let timer: ReturnType<typeof setTimeout> | undefined
onUnmounted(() => {
  stopped = true
  clearTimeout(timer)
})

async function load() {
  try {
    const value = await api<Migration>(`/nodes/placement/${props.runId}/backend-migration`)
    if (stopped)
      return
    if (state.value?.pending && !value.pending && value.fromBackend === state.value.toBackend && value.fromBackend !== state.value.fromBackend) {
      completed.value = true
      emit('completed')
    }

    state.value = value
    if (!value.nodes.some(node => node.id === destination.value))
      destination.value = value.nodes[0]?.id || ''
    error.value = ''
  }
  catch (e) {
    // Older installations and non-owner views do not expose migration controls.
    if (state.value)
      error.value = e instanceof Error ? e.message : 'Unable to read migration progress'
  }

  if (!stopped && state.value?.pending)
    timer = setTimeout(load, 1500)
}

void load()

async function migrate() {
  if (!state.value)
    return
  busy.value = true
  error.value = ''
  try {
    await api(`/nodes/placement/${props.runId}/backend-migration`, {
      method: 'POST',
      body: JSON.stringify({ nodeId: destination.value, backend: state.value.toBackend, confirm: true }),
    })
    state.value.pending = true
    confirm.value = false
    await load()
  }
  catch (e) { error.value = e instanceof Error ? e.message : 'Unable to migrate this conversation' }
  finally { busy.value = false }
}
</script>

<template>
  <section v-if="state?.required || state?.optional || state?.pending || completed" class="mt-3 border-t border-line pt-3" aria-label="Change execution backend">
    <h3 class="m-0! text-xs! font-semibold text-ink">
      Change execution backend
    </h3>
    <template v-if="completed">
      <p role="status">
        Migration complete. Send a new message to continue in the same conversation.
      </p>
      <UiButton v-if="state?.optional" size="small" @click="completed = false">
        Change environment again
      </UiButton>
    </template>
    <p v-else-if="state?.pending" role="status">
      Migrating to {{ label }}.
      <template v-if="state.environmentExpired">
        The next message will create an empty environment. Conversation history and published files are retained.
      </template>
      <template v-else>
        Execution is stopped while the environment is saved and transferred. You can leave this screen.
      </template>
    </p>
    <template v-else>
      <p v-if="state?.environmentExpired">
        The previous environment expired. Confirm {{ label }} to continue in the same conversation with an empty environment. History, uploads and published artifacts are retained.
      </p>
      <p v-else-if="state?.optional">
        You can explicitly change this conversation to {{ label }}. Its current environment is kept until you confirm.
      </p>
      <p v-else>
        The agent now uses {{ label }}. Migrate this conversation to keep its history and files.
      </p>
      <p v-if="state?.reason" class="text-warning">
        {{ state.reason }}
      </p>
      <template v-else-if="options.length">
        <VirtualSelect
          v-model="destination"
          :options="options"
          label="Migration destination"
          compact
        />
        <UiButton
          class="mt-2"
          size="small"
          :disabled="busy || !destination"
          @click="confirm = true"
        >
          Migrate this conversation…
        </UiButton>
      </template>
      <p v-else>
        Authorize a destination node on the agent's page before migrating.
      </p>
    </template>
    <p v-if="error || state?.error" role="alert" class="text-coral">
      {{ error || state?.error }}
    </p>
    <Modal v-if="confirm" title="Migrate this conversation?" @close="confirm = false">
      <div class="p-6 grid gap-4">
        <p v-if="state?.environmentExpired">
          The previous working files were deleted after inactivity. Confirm {{ label }} for a new empty environment, keeping the same conversation, history, uploads and published artifacts.
        </p>
        <p v-else>
          Execution will stop and its files will move to {{ label }}. This keeps the same conversation and history.
        </p>
        <p>The next message starts a fresh engine session with the conversation context. Operating-system tools and dependencies may need reinstalling.</p>
        <p v-if="!state?.environmentExpired">
          The original environment is retained if migration fails.
        </p>
        <p v-if="state?.environmentExpired">
          Your chosen node is pinned for the new environment. You can change this in the conversation's node preferences.
        </p>
        <p v-else>
          The conversation's previous node preference will be cleared.
        </p>
        <div class="mt-4 flex justify-end gap-2">
          <UiButton :disabled="busy" @click="confirm = false">
            Cancel
          </UiButton>
          <UiButton variant="primary" :disabled="busy" @click="migrate">
            {{ state?.environmentExpired ? 'Confirm backend change' : 'Stop and migrate' }}
          </UiButton>
        </div>
      </div>
    </Modal>
  </section>
</template>
