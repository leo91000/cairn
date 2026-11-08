<script setup lang="ts">
import { reloadWebApp, webUpdate } from '../web-update'
import UiButton from './UiButton.vue'
import UiNotice from './UiNotice.vue'

defineProps<{ required?: boolean }>()
</script>

<template>
  <aside v-if="required || webUpdate.available || webUpdate.loadFailed" class="fixed inset-x-3 top-3 z-[100] mx-auto max-w-xl" aria-label="Cairn update">
    <UiNotice role="alert" tone="warning" :title="required ? 'Update required.' : webUpdate.loadFailed ? 'This page couldn’t load.' : 'A new version of Cairn is ready.'">
      <p v-if="required">
        Reload Cairn to use the current web app. Your conversation drafts and destination are kept.
        If this message remains after reloading, update your installation before continuing.
      </p>
      <p v-else-if="webUpdate.loadFailed && !webUpdate.available">
        Cairn may have been updated, or the connection was interrupted.
      </p>
      <p v-if="!required">
        Save any unsaved changes, then reload to continue.
      </p>
      <UiButton size="small" class="mt-2" @click="reloadWebApp">
        Reload Cairn
      </UiButton>
    </UiNotice>
  </aside>
</template>
