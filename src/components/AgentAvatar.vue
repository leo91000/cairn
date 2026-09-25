<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { state } from '../api'
import {
  Clock,
  MessageCircleQuestion,
  Pause,
  X,
} from '../icons'
import { identityColor, initial } from '../signal'
import Icon from './Icon.vue'

// Agent identity: a rounded square in the agent's colour. A comet arc orbits it while the
// agent works (overflowing, so rows stay aligned) and a small badge marks what needs the user.
const props = withDefaults(defineProps<{
  name: string
  identity?: string
  size?: number
  working?: boolean
  badge?: 'question' | 'failed' | 'paused' | 'queued' | null
}>(), {
  identity: '',
  size: 36,
  working: false,
  badge: null,
})
const source = computed(() => state.agents.find(agent => agent.id === props.identity)?.avatar?.url || '')
const failed = ref(false)
watch(source, () => {
  failed.value = false
})
const radius = computed(() => Math.round(props.size * 0.3))
</script>

<template>
  <span class="agent-avatar relative inline-grid shrink-0 place-items-center" :style="{ 'width': `${size}px`, 'height': `${size}px`, '--avatar-radius': `${radius}px` }" aria-hidden="true">
    <span v-if="working" class="avatar-orbit" />
    <span class="grid size-full overflow-hidden select-none place-items-center font-heading font-bold text-white" :style="{ background: identityColor(identity || name), borderRadius: `${radius}px`, fontSize: `${Math.round(size * 0.42)}px` }">
      <img
        v-if="source && !failed"
        :src="source"
        alt=""
        class="size-full object-cover"
        @error="failed = true"
      >
      <template v-else>{{ initial(name) }}</template>
    </span>
    <span
      v-if="badge"
      class="absolute -bottom-1 -right-1 grid size-[15px] place-items-center rounded-full ring-2 ring-surface"
      :class="badge === 'question' || badge === 'failed' ? 'bg-coral text-white' : 'bg-muted text-surface'"
    >
      <Icon :name="badge === 'question' ? MessageCircleQuestion : badge === 'failed' ? X : badge === 'paused' ? Pause : Clock" :size="badge === 'question' || badge === 'failed' ? 10 : 9" />
    </span>
  </span>
</template>
