<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { getConversationRetention } from '../../../../packages/contracts/generated/getConversationRetention'
import { setConversationRetention } from '../../../../packages/contracts/generated/setConversationRetention'
import { api, notify } from '../api'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const days = ref<number | string>(7)
const loaded = ref(false)
const busy = ref(false)
const confirming = ref(false)
const error = ref('')
const valid = computed(() => days.value !== '' && Number.isInteger(Number(days.value)) && Number(days.value) >= 0 && Number(days.value) <= 36500)

onMounted(async () => {
  try {
    days.value = (await getConversationRetention(api, {})).inactivityDays
    loaded.value = true
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
})

async function save() {
  busy.value = true
  error.value = ''
  try {
    const policy = await setConversationRetention(api, { inactivityDays: Number(days.value) })
    days.value = policy.inactivityDays
    confirming.value = false
    notify('Conversation retention saved')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="panel border-b border-line settings-section mb-5.5 px-0 py-7 phone:py-5">
    <h2>Automatic environment cleanup</h2>
    <p class="text-sm text-muted my-3">
      Remove working environments after this many days without activity, including existing conversations. History, uploaded attachments, all published file versions and shared links are retained. Unpushed changes, tools stored in the environment and other working files are permanently removed. The next message starts a fresh environment and informs the agent. Use 0 to disable.
    </p>
    <p class="text-sm text-muted my-3">
      Running agents, pending messages, open questions and scheduled wakeups are protected. The delay starts from the latest conversation activity or the end of the agent’s work. Reading a conversation does not restart the delay.
    </p>
    <UiAlert v-if="error && !confirming" class="my-3">
      {{ error }}
    </UiAlert>
    <form v-if="loaded" class="grid gap-4 max-w-xl" @submit.prevent="confirming = true">
      <label>Days of inactivity<input
        v-model="days"
        type="number"
        min="0"
        max="36500"
        step="1"
        required
        :disabled="busy"
      ></label>
      <UiButton type="submit" :disabled="busy || !valid">
        Save conversation retention
      </UiButton>
    </form>
    <Modal v-if="confirming" title="Save conversation retention?" @close="!busy && (confirming = false)">
      <p class="my-4">
        {{ Number(days) === 0 ? 'Automatic environment cleanup will be disabled. Working files already removed cannot be recovered.' : `Working environments inactive for ${days} days will be permanently removed, including existing conversations. History, uploaded attachments and published files are retained. Unpushed changes cannot be recovered.` }}
      </p>
      <UiAlert v-if="error" class="my-3">
        {{ error }}
      </UiAlert>
      <div class="flex gap-3 my-4">
        <UiButton :disabled="busy" @click="confirming = false">
          Cancel
        </UiButton>
        <UiButton :disabled="busy" @click="save">
          Confirm retention
        </UiButton>
      </div>
    </Modal>
  </section>
</template>
