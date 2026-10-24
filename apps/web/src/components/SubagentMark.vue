<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ActivityArtifact } from '../activity'
import { computed } from 'vue'
import { brands } from '../accounts'
import ProviderMark from './ProviderMark.vue'
import SubagentAvatar from './SubagentAvatar.vue'

// A delegated task shows the coding agent it runs on; a native subagent, its own identity.
const props = withDefaults(defineProps<{ agent: ActivityArtifact, size?: number }>(), { size: 28 })
const provider = computed(() => {
  const value = props.agent.subagent?.delegation?.provider
  return value && value in brands ? value as Provider : undefined
})
</script>

<template>
  <ProviderMark v-if="provider" :provider="provider" :size="size" />
  <SubagentAvatar v-else :agent="agent" :size="size" />
</template>
