<script setup lang="ts">
import type { ChatPlanSettings } from '../../../../packages/contracts/generated/typescript/ChatPlanSettings'
import { onMounted, ref } from 'vue'
import { getChatPlanSettings } from '../../../../packages/contracts/generated/getChatPlanSettings'
import { saveChatPlanSettings } from '../../../../packages/contracts/generated/saveChatPlanSettings'
import { api, notify } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const settings = ref<ChatPlanSettings>()
const busy = ref(false)
const error = ref('')

async function load() {
  error.value = ''
  try {
    settings.value = await getChatPlanSettings(api, {})
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
}

async function toggle() {
  if (!settings.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    settings.value = await saveChatPlanSettings(api, { ...settings.value, enabled: !settings.value.enabled })
    notify('Chat plan settings saved')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}

onMounted(load)
</script>

<template>
  <section class="panel border-b border-line settings-section mb-5.5 px-0 py-7 phone:py-5">
    <h2>Chat Plan mode</h2>
    <p class="text-sm text-muted my-3">
      Allow Plan in the chat composer for this installation. Disabled by default. With Codex or Claude Code, Plan explores and proposes an approach before you choose to implement it. Already accepted plan requests keep their restrictions when disabled.
    </p>
    <UiAlert v-if="error" class="my-3">
      {{ error }}
      <UiButton :disabled="busy" @click="load">
        Reload chat plan settings
      </UiButton>
    </UiAlert>
    <UiButton
      v-if="settings"
      role="switch"
      :aria-checked="settings.enabled"
      :disabled="busy"
      @click="toggle"
    >
      {{ settings.enabled ? 'Disable chat Plan mode' : 'Enable chat Plan mode' }}
    </UiButton>
  </section>
</template>
