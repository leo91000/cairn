<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import { ref } from 'vue'
import { deleteChat } from '../../../../packages/contracts/generated/deleteChat'
import { api, state } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ chat: ChatView }>()
const emit = defineEmits<{ close: [], deleted: [id: string] }>()
const deleting = ref(false)
const error = ref('')

async function remove() {
  if (deleting.value)
    return
  deleting.value = true
  error.value = ''
  try {
    await deleteChat(api, { chatId: props.chat.id, confirm: true })
    const draftScope = state.installationId ? `${state.installationId}:` : ''
    sessionStorage.removeItem(`cairn-chat-draft:${draftScope}${props.chat.id}`)
    emit('deleted', props.chat.id)
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally { deleting.value = false }
}
</script>

<template>
  <Modal title="Delete conversation?" @close="!deleting && emit('close')">
    <div class="p-6 phone:p-5">
      <p class="mb-3 text-sm font-semibold wrap-anywhere">
        {{ chat.title }}
      </p>
      <p class="mb-3 text-sm text-muted">
        This permanently deletes the conversation, its messages and working files. You cannot undo this.
      </p>
      <p class="mb-5 text-sm text-muted">
        Any work in progress will stop, pending messages and questions will be cancelled, and public links will stop working immediately.
      </p>
      <p v-if="error" role="alert" class="mb-4 text-sm text-danger">
        {{ error }}
      </p>
      <div class="flex justify-end gap-3">
        <UiButton :disabled="deleting" autofocus @click="emit('close')">
          Keep conversation
        </UiButton>
        <UiButton :disabled="deleting" variant="danger" @click="remove()">
          {{ deleting ? 'Deleting…' : 'Delete now' }}
        </UiButton>
      </div>
    </div>
  </Modal>
</template>
