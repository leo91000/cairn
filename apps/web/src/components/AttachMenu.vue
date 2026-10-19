<script setup lang="ts">
import type { ClassValue } from 'vue'
import { ref } from 'vue'
import {
  Camera,
  Images,
  Paperclip,
  Plus,
} from '../icons'
import Icon from './Icon.vue'
import MenuButton from './MenuButton.vue'

// The composer's ＋ button: photos, the camera (on devices that have one) or any file.
defineProps<{ disabled?: boolean, triggerClass?: ClassValue }>()
const emit = defineEmits<{ files: [files: File[]] }>()
const photos = ref<HTMLInputElement>()
const camera = ref<HTMLInputElement>()
const files = ref<HTMLInputElement>()

function picked(event: Event) {
  const input = event.target as HTMLInputElement
  const selected = Array.from(input.files ?? [])
  input.value = ''
  if (selected.length)
    emit('files', selected)
}
</script>

<template>
  <input
    ref="photos"
    type="file"
    accept="image/*"
    multiple
    class="hidden"
    aria-label="Attach photos"
    :disabled="disabled"
    @change="picked"
  >
  <input
    ref="camera"
    type="file"
    accept="image/*"
    capture="environment"
    class="hidden"
    aria-label="Take a photo"
    :disabled="disabled"
    @change="picked"
  >
  <input
    ref="files"
    type="file"
    multiple
    class="hidden"
    aria-label="Attach files"
    :disabled="disabled"
    @change="picked"
  >
  <MenuButton
    label="Add images or files"
    title="Add images or files · up to 8 files, 10 MB each"
    menu-label="Attach"
    :disabled="disabled"
    :trigger-class="triggerClass"
    placement="above"
    align="start"
    :width="200"
  >
    <template #trigger>
      <Icon :name="Plus" :size="18" />
    </template>
    <template #default="{ select }">
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(() => photos?.click())"
      >
        <Icon :name="Images" :size="16" />Photos
      </button>
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(() => camera?.click())"
      >
        <Icon :name="Camera" :size="16" />Camera
      </button>
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        class="menu-item"
        @click="select(() => files?.click())"
      >
        <Icon :name="Paperclip" :size="16" />Files
      </button>
    </template>
  </MenuButton>
</template>
