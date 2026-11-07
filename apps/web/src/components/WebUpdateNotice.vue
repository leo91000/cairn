<script setup lang="ts">
import { reloadWebApp, webUpdate } from '../web-update'
import UiButton from './UiButton.vue'
import UiNotice from './UiNotice.vue'
</script>

<template>
  <aside v-if="webUpdate.available || webUpdate.loadFailed" class="fixed inset-x-3 top-3 z-[100] mx-auto max-w-xl" aria-label="Cairn update">
    <UiNotice role="alert" tone="warning" :title="webUpdate.loadFailed ? 'This page couldn’t load.' : 'A new version of Cairn is ready.'">
      <p v-if="webUpdate.loadFailed && !webUpdate.available">
        Cairn may have been updated, or the connection was interrupted.
      </p>
      <p>Save any unsaved changes, then reload to continue.</p>
      <UiButton size="small" class="mt-2" @click="reloadWebApp">
        Reload Cairn
      </UiButton>
    </UiNotice>
  </aside>
</template>
