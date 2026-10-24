<script setup lang="ts">
import UiNotice from './UiNotice.vue'
</script>

<template>
  <UiNotice role="alert" tone="danger" class="error">
    <slot />
  </UiNotice>
</template>
