<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import { computed, ref } from 'vue'
import { renameChat } from '../../../../packages/contracts/generated/renameChat'
import { api } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

// `chat` follows the live conversation, so a retry sends the revision the person now sees.
const props = defineProps<{ chat: ChatView }>()

const emit = defineEmits<{ close: [], renamed: [chat: ChatView] }>()

// The installation trims the title and keeps it within 90 characters without control characters.
const MAX_TITLE = 90

const title = ref(props.chat.title)
const saving = ref(false)
const error = ref('')
const trimmed = computed(() => title.value.trim())
// Counted in characters like the installation, so emoji and longer existing titles stay editable.
const length = computed(() => [...trimmed.value].length)
const tooLong = computed(() => length.value > MAX_TITLE)
const changed = computed(() => !!trimmed.value && !tooLong.value && trimmed.value !== props.chat.title)

async function save() {
  if (saving.value || !changed.value)
    return
  saving.value = true
  error.value = ''
  try {
    emit('renamed', await renameChat(api, { chatId: props.chat.id, revision: props.chat.revision, title: trimmed.value }))
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally { saving.value = false }
}
</script>

<template>
  <Modal title="Rename conversation" @close="!saving && emit('close')">
    <form class="grid gap-4 p-6 phone:p-5" @submit.prevent="save()">
      <label>Title<input
        v-model="title"
        required
        autocomplete="off"
        :aria-invalid="tooLong || undefined"
        :aria-describedby="tooLong ? 'rename-too-long' : undefined"
        :disabled="saving"
        autofocus
        @focus="($event.target as HTMLInputElement).select()"
      ></label>
      <p v-if="tooLong" id="rename-too-long" class="m-0! text-sm text-danger">
        Use at most {{ MAX_TITLE }} characters ({{ length }} now).
      </p>
      <p class="m-0! text-sm text-muted">
        Cairn stops naming this conversation automatically once you rename it.
      </p>
      <p v-if="error" role="alert" class="m-0! text-sm text-danger">
        {{ error }}
      </p>
      <div class="flex justify-end gap-3">
        <UiButton :disabled="saving" @click="emit('close')">
          Cancel
        </UiButton>
        <UiButton type="submit" variant="primary" :disabled="saving || !changed">
          {{ saving ? 'Saving…' : 'Save' }}
        </UiButton>
      </div>
    </form>
  </Modal>
</template>
