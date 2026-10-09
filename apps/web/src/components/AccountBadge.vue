<script setup lang="ts">
import type { Account } from '../../../../packages/contracts/accounts'
import { computed } from 'vue'
import { resetsIn, statusLabels, statusTones } from '../accounts'
import { CircleAlert, Pause } from '../icons'
import Icon from './Icon.vue'

// The account's status as a small pill. A ready account shows none.
const props = defineProps<{ account: Account }>()
const label = computed(() => {
  const { status, resetsAt } = props.account
  if (status === 'waiting' && resetsAt)
    return `Resets ${resetsIn(resetsAt)}`
  return statusLabels[status]
})
const tones = {
  accent: 'bg-soft text-accent',
  warning: 'bg-warning-surface text-warning',
  muted: 'bg-hover text-muted',
  attention: 'bg-coral-soft text-coral',
}
</script>

<template>
  <span v-if="label" class="account-badge inline-flex h-[22px] shrink-0 items-center gap-[5px] rounded-full px-2.5 text-[11.5px] font-semibold whitespace-nowrap" :class="tones[statusTones[account.status]]">
    <span v-if="account.status === 'next'" class="size-1.5 rounded-full bg-current" />
    <Icon v-else-if="account.status === 'paused'" :name="Pause" :size="11" />
    <Icon v-else-if="statusTones[account.status] === 'attention'" :name="CircleAlert" :size="12" />
    {{ label }}
  </span>
</template>
