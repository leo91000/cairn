<script setup lang="ts">
import { useRoute } from 'vue-router'
import RunWorkspace from '../components/RunWorkspace.vue'

const route = useRoute()
</script>

<template>
  <RunWorkspace :key="route.params.id as string" :run-id="route.params.id as string" />
</template>
