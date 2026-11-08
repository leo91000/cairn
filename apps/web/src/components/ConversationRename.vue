<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import { computed, nextTick, ref } from 'vue'
import { renameChat } from '../../../../packages/contracts/generated/renameChat'
import { api, ApiError } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ chat: ChatView }>()

const emit = defineEmits<{ close: [], renamed: [chat: ChatView, from: string] }>()

// The dialog renames the conversation as it was when opened: a title changed meanwhile is
// reported by the installation instead of being overwritten.
const { id: chatId, revision, title: shown } = props.chat

// The installation trims the title and keeps it within 90 characters without control characters.
const MAX_TITLE = 90

const title = ref(shown)
const field = ref<HTMLInputElement>()
const saving = ref(false)
const error = ref('')
// Once the installation reports a change, saving again cannot succeed until the dialog reopens.
const stale = ref(false)
const trimmed = computed(() => title.value.trim())
// Counted in characters like the installation, so emoji and longer existing titles stay editable.
const length = computed(() => [...trimmed.value].length)
const tooLong = computed(() => length.value > MAX_TITLE)
const changed = computed(() => !!trimmed.value && !tooLong.value && trimmed.value !== shown)

async function save() {
  if (saving.value || stale.value || !changed.value)
    return
  saving.value = true
  error.value = ''
  try {
    emit('renamed', await renameChat(api, { chatId, revision, title: trimmed.value }), revision)
    return
  }
  catch (e) {
    error.value = (e as Error).message
    stale.value = e instanceof ApiError && e.status === 409
  }
  finally { saving.value = false }

  // The field was disabled while saving; the person corrects it from there.
  await nextTick()
  field.value?.focus()
}
</script>

<template>
  <Modal title="Rename conversation" @close="!saving && emit('close')">
    <form class="grid gap-4 p-6 phone:p-5" @submit.prevent="save()">
      <label>Title<input
        ref="field"
        v-model="title"
        required
        autocomplete="off"
        :aria-invalid="tooLong || undefined"
        :aria-describedby="tooLong ? 'rename-too-long rename-hint' : 'rename-hint'"
        :disabled="saving"
        autofocus
        @focus="($event.target as HTMLInputElement).select()"
      ></label>
      <p v-if="tooLong" id="rename-too-long" class="m-0! text-sm text-danger">
        Use at most {{ MAX_TITLE }} characters ({{ length }} now).
      </p>
      <p id="rename-hint" class="m-0! text-sm text-muted">
        Cairn stops naming this conversation automatically once you rename it.
      </p>
      <p v-if="error" role="alert" class="m-0! text-sm text-danger">
        {{ error }}
      </p>
      <div class="flex justify-end gap-3">
        <UiButton :disabled="saving" @click="emit('close')">
          Cancel
        </UiButton>
        <UiButton type="submit" variant="primary" :disabled="saving || stale || !changed">
          {{ saving ? 'Saving…' : 'Save' }}
        </UiButton>
      </div>
    </form>
  </Modal>
</template>
