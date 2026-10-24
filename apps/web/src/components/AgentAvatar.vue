<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import { computed, ref, watch } from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
import { brands } from '../accounts'
import { apiResourceUrl, state } from '../api'
import {
  Check,
  Clock,
  Hourglass,
  MessageCircleQuestion,
  Pause,
  X,
} from '../icons'
import { identityColor, initial } from '../signal'
import Icon from './Icon.vue'

// Agent identity: a rounded square in the agent's colour. A comet arc orbits it while the
// agent works (overflowing, so rows stay aligned) and a small badge marks what needs the user
// or finished work they have not read. A conversation's header adds the brand of the coding
// agent of its latest turn in the corner the badge would use, which then moves to the top.
const props = withDefaults(defineProps<{
  name: string
  identity?: string
  size?: number
  working?: boolean
  badge?: 'question' | 'failed' | 'done' | 'paused' | 'queued' | 'waiting' | null
  harness?: Provider | null
}>(), {
  identity: '',
  size: 36,
  working: false,
  badge: null,
  harness: null,
})
const source = computed(() => apiResourceUrl(state.agents.find(agent => agent.id === props.identity)?.avatar?.url || ''))
const failed = ref(false)
watch(source, () => {
  failed.value = false
})
const radius = computed(() => Math.round(props.size * 0.3))
const mark = computed(() => Math.max(14, Math.round(props.size * 0.44)))
</script>

<template>
  <span
    class="agent-avatar relative inline-grid shrink-0 place-items-center"
    :style="{ 'width': `${size}px`, 'height': `${size}px`, '--avatar-radius': `${radius}px` }"
    :aria-hidden="harness ? undefined : true"
    :role="harness ? 'img' : undefined"
    :aria-label="harness ? `Latest harness: ${providers[harness].label}` : undefined"
  >
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
      v-if="harness"
      class="absolute -bottom-1 -right-1 grid place-items-center rounded-[30%] ring-2 ring-surface"
      :class="brands[harness].tile"
      :style="{ width: `${mark}px`, height: `${mark}px` }"
      :data-harness="harness"
    >
      <Icon :name="brands[harness].icon" :size="Math.round(mark * 0.62)" />
    </span>
    <span
      v-if="badge"
      class="absolute -right-1 grid size-[15px] place-items-center rounded-full ring-2 ring-surface"
      :class="[harness ? '-top-1' : '-bottom-1', badge === 'question' || badge === 'failed' ? 'bg-coral text-white' : badge === 'done' ? 'bg-success text-white' : badge === 'waiting' ? 'bg-accent text-surface' : 'bg-muted text-surface']"
    >
      <Icon :name="badge === 'question' ? MessageCircleQuestion : badge === 'failed' ? X : badge === 'done' ? Check : badge === 'paused' ? Pause : badge === 'waiting' ? Hourglass : Clock" :size="badge === 'question' || badge === 'failed' || badge === 'done' ? 10 : 9" />
    </span>
  </span>
</template>
