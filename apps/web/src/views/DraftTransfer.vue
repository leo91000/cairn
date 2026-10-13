<script setup lang="ts">
import { onMounted } from 'vue'
import { installationHost } from '../beacon-location'
import { receiveInstallationDrafts } from '../draft-transfer'

onMounted(() => {
  if (installationHost && window.parent !== window)
    void receiveInstallationDrafts(installationHost)
})
</script>

<template>
  <span />
</template>
