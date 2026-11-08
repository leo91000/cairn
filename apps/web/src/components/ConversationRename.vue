<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import { computed, nextTick, ref } from 'vue'
import { getChat } from '../../../../packages/contracts/generated/getChat'
import { renameChat } from '../../../../packages/contracts/generated/renameChat'
import { api } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ chat: ChatView }>()

const emit = defineEmits<{ close: [], renamed: [chat: ChatView] }>()

// The title the dialog opened on. A message or a turn meanwhile leaves the conversation renamable;
// a title changed elsewhere is never overwritten from this dialog.
const shown = props.chat.title
// Found when saving, before the live conversation shows it.
const found = ref<string>()
const renamedElsewhere = computed(() => props.chat.title !== shown ? props.chat.title : found.value)

// The installation trims the title and keeps it within 90 characters without control characters.
const MAX_TITLE = 90

const title = ref(shown)
const field = ref<HTMLInputElement>()
const saving = ref(false)
const error = ref('')
const trimmed = computed(() => title.value.trim())
// Counted in characters like the installation, so emoji and longer existing titles stay editable.
const length = computed(() => [...trimmed.value].length)
const tooLong = computed(() => length.value > MAX_TITLE)
const changed = computed(() => !!trimmed.value && !tooLong.value && trimmed.value !== shown)

async function save() {
  if (saving.value || renamedElsewhere.value !== undefined || !changed.value)
    return
  const chatId = props.chat.id
  saving.value = true
  error.value = ''
  try {
    // Messages and turns change the revision too, and the live conversation can trail it.
    const current = await getChat(api, { chatId })
    if (current.title !== shown) {
      found.value = current.title
      return
    }

    emit('renamed', await renameChat(api, { chatId, revision: current.revision, title: trimmed.value }))
    return
  }
  catch (e) {
    error.value = (e as Error).message
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
        @focus.once="($event.target as HTMLInputElement).select()"
      ></label>
      <p v-if="tooLong" id="rename-too-long" class="m-0! text-sm text-danger">
        Use at most {{ MAX_TITLE }} characters ({{ length }} now).
      </p>
      <p id="rename-hint" class="m-0! text-sm text-muted">
        Cairn stops naming this conversation automatically once you rename it.
      </p>
      <p v-if="renamedElsewhere !== undefined" role="alert" class="m-0! text-sm text-danger">
        Renamed “{{ renamedElsewhere }}” elsewhere meanwhile. Reopen this dialog to rename it again.
      </p>
      <p v-else-if="error" role="alert" class="m-0! text-sm text-danger">
        {{ error }}
      </p>
      <div class="flex justify-end gap-3">
        <UiButton :disabled="saving" @click="emit('close')">
          Cancel
        </UiButton>
        <UiButton type="submit" variant="primary" :disabled="saving || renamedElsewhere !== undefined || !changed">
          {{ saving ? 'Saving…' : 'Save' }}
        </UiButton>
      </div>
    </form>
  </Modal>
</template>
