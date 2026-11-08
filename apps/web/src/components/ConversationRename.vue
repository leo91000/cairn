<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import {
  computed,
  nextTick,
  ref,
  watch,
} from 'vue'
import { getChat } from '../../../../packages/contracts/generated/getChat'
import { renameChat } from '../../../../packages/contracts/generated/renameChat'
import { api } from '../api'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ chat: ChatView }>()

const emit = defineEmits<{ close: [], renamed: [chat: ChatView] }>()

// The title the dialog opened on. Nothing that happens meanwhile blocks the rename, but a title
// that changed (automatically or elsewhere) is never replaced unseen: the dialog names it first.
const shown = props.chat.title
// A newer title found when saving, until the live conversation brings its own.
const found = ref<string>()
watch(() => props.chat.title, () => found.value = undefined)
const current = computed(() => found.value ?? props.chat.title)
const meanwhile = computed(() => current.value !== shown ? current.value : undefined)

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
  if (saving.value || !changed.value)
    return
  const chatId = props.chat.id
  saving.value = true
  error.value = ''
  try {
    // Messages and turns change the revision too, and the live conversation can trail it.
    const latest = await getChat(api, { chatId })
    if (latest.title !== current.value) {
      found.value = latest.title
      return
    }

    emit('renamed', await renameChat(api, { chatId, revision: latest.revision, title: trimmed.value }))
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
      <p v-if="meanwhile !== undefined" role="alert" class="m-0! text-sm text-danger">
        The title changed to “{{ meanwhile }}” meanwhile; saving replaces it.
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
