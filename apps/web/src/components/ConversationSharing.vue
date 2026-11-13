<script setup lang="ts">
import type { ChatShare } from '../../../../packages/contracts/generated/typescript/ChatShare'
import { onBeforeUnmount, ref } from 'vue'
import { getChatShare } from '../../../../packages/contracts/generated/getChatShare'
import { revokeChatShare } from '../../../../packages/contracts/generated/revokeChatShare'
import { setChatShare } from '../../../../packages/contracts/generated/setChatShare'
import { api, notify } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ chatId: string }>()
const emit = defineEmits<{ close: [] }>()
const share = ref<ChatShare>()
const hours = ref(24)
const busy = ref(true)
const error = ref('')
const controller = new AbortController()
onBeforeUnmount(() => controller.abort())

function request<T>(path: string, options: RequestInit) {
  return api<T>(path, { ...options, signal: controller.signal })
}

async function update(action: 'get' | 'set' | 'revoke') {
  if (action !== 'get' && busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    const input = { chatId: props.chatId }
    share.value = action === 'get'
      ? await getChatShare(request, input)
      : action === 'revoke'
        ? await revokeChatShare(request, input)
        : await setChatShare(request, { ...input, expiresInHours: hours.value })
  }
  catch (cause) {
    if (!controller.signal.aborted)
      error.value = (cause as Error).message
  }
  finally { busy.value = false }
}

async function copy() {
  try {
    await navigator.clipboard.writeText(share.value!.publicUrl!)
    notify('Conversation link copied.')
  }
  catch { error.value = 'Could not copy the link. Select and copy it below.' }
}

void update('get')
</script>

<template>
  <Modal title="Share conversation" @close="emit('close')">
    <div class="space-y-4">
      <p>Anyone with this link can read the conversation and every published file, including future messages and files until it expires. No sign-in is required.</p>
      <p class="text-sm text-muted">
        This page asks search engines not to index it. Keep the link private to its recipients.
      </p>
      <label class="flex items-center gap-3 text-sm">
        Expires after (hours)
        <input
          v-model.number="hours"
          type="number"
          min="1"
          max="720"
          step="1"
          class="w-24 rounded-lg border border-line bg-surface px-3 py-2"
          :disabled="busy"
        >
      </label>
      <p class="text-xs text-muted">
        24 hours by default · From 1 hour to 30 days.
      </p>
      <template v-if="share?.publicUrl">
        <p class="text-sm">
          Expires {{ new Date(share.expiresAt!).toLocaleString() }}
        </p>
        <input
          :value="share.publicUrl"
          aria-label="Conversation public link"
          readonly
          class="w-full rounded-lg border border-line bg-surface px-3 py-2 text-sm"
          @focus="($event.target as HTMLInputElement).select()"
        >
        <div class="flex flex-wrap gap-2">
          <UiButton :disabled="busy" @click="copy">
            Copy link
          </UiButton>
          <UiButton :disabled="busy" @click="update('revoke')">
            Revoke link
          </UiButton>
        </div>
      </template>
      <UiButton :disabled="busy || !share || !Number.isInteger(hours) || hours < 1 || hours > 720" @click="update('set')">
        {{ busy ? 'Loading…' : share?.publicUrl ? 'Replace link with this expiration' : 'Create link' }}
      </UiButton>
      <p v-if="share?.publicUrl" class="text-xs text-muted">
        Replacing or revoking the link blocks further access. Downloaded copies remain with their recipients.
      </p>
      <p v-if="error" role="alert" class="text-sm text-danger">
        {{ error }}
      </p>
      <UiButton v-if="error && !share" :disabled="busy" @click="update('get')">
        Retry
      </UiButton>
    </div>
  </Modal>
</template>
