<script setup lang="ts">
import type { Account } from '../../../../packages/contracts/accounts'
import { computed } from 'vue'
import { generalWindows, remainingPercent } from '../../../../packages/contracts/accounts'
import { windowName } from '../accounts'
import { ChevronRight } from '../icons'
import AccountBadge from './AccountBadge.vue'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import UsageBar from './UsageBar.vue'

// One account: who it is, the usage it has left, what runs on it and whether it needs you.
const props = defineProps<{ account: Account, selected?: boolean }>()
defineEmits<{ open: [] }>()
const windows = computed(() => generalWindows(props.account.usage).slice(0, 2))
const running = computed(() => props.account.activeRunIds.length)
const paused = computed(() => props.account.status === 'paused')
// The badge already says when the account needs signing in.
const note = computed(() => {
  const { state, usage } = props.account
  if (state !== 'ready')
    return ''
  return usage?.error ? 'Usage unavailable' : 'Usage not reported yet'
})
</script>

<template>
  <button
    type="button"
    class="account-row group grid w-full grid-cols-[minmax(0,1fr)_15rem_5.75rem_8.5rem_1rem] items-center gap-4 border-t border-line px-4.5 py-3.5 text-left transition-colors hover:bg-hover/40 focus-visible:-outline-offset-2 focus-visible:outline-2 focus-visible:outline-accent phone:grid-cols-[minmax(0,1fr)_auto] phone:gap-x-3 phone:gap-y-2.5 phone:px-4"
    :class="{ 'bg-soft/40': selected }"
    :aria-label="`${account.name}, ${account.email ?? 'not signed in'}`"
    @click="$emit('open')"
  >
    <span class="flex min-w-0 items-center gap-3">
      <AgentAvatar
        :name="account.name"
        :identity="account.id"
        :size="36"
        :working="running > 0"
      />
      <span class="min-w-0">
        <span class="flex min-w-0 items-center gap-2">
          <span class="truncate text-[14px] font-semibold">{{ account.name }}</span>
          <span class="hidden phone:inline-flex"><AccountBadge :account="account" /></span>
        </span>
        <span class="block truncate text-xs text-muted">{{ account.email ?? 'Not signed in' }}<template v-if="account.plan"> · {{ account.plan }}</template></span>
      </span>
    </span>
    <span v-if="windows.length" class="grid gap-[7px] phone:order-last phone:col-span-2 phone:grid-cols-2 phone:gap-2" :class="{ 'opacity-55': paused }">
      <span v-for="window in windows" :key="window.id" class="grid grid-cols-[2.6rem_minmax(0,1fr)_2.3rem] items-center gap-2 text-[11.5px] phone:grid-cols-1">
        <span class="text-muted phone:hidden">{{ windowName(window) }}</span>
        <UsageBar :remaining="remainingPercent(window)" :muted="paused || account.stale" :label="`${account.name} ${window.label} remaining`" />
        <span class="text-right font-semibold tabular-nums phone:hidden">{{ Math.round(remainingPercent(window)) }}%</span>
      </span>
    </span>
    <span v-else class="truncate text-xs text-muted phone:hidden">{{ note }}</span>
    <span class="flex flex-col gap-1 text-[11.5px] text-muted phone:hidden">
      <span v-if="account.maxConcurrentRuns <= 8" class="inline-flex items-center gap-[3px]" :aria-label="`${running} of ${account.maxConcurrentRuns} parallel runs`">
        <span
          v-for="slot in account.maxConcurrentRuns"
          :key="slot"
          class="size-[7px] rounded-full"
          :class="slot <= running ? 'bg-accent ring-3 ring-accent/15' : 'bg-hover ring-1 ring-line'"
        />
      </span>
      <span v-else class="tabular-nums">{{ running }} / {{ account.maxConcurrentRuns }}</span>
      <span v-if="running">{{ running }} running</span>
    </span>
    <span class="justify-self-end phone:hidden">
      <AccountBadge :account="account" />
    </span>
    <Icon :name="ChevronRight" :size="16" class="text-subtle transition-transform group-hover:translate-x-0.5 phone:hidden" />
    <span class="hidden text-right phone:block">
      <span class="block font-heading text-[19px] leading-none font-extrabold tracking-[-0.02em] tabular-nums" :class="paused ? 'text-muted' : 'text-ink'">{{ account.remainingPercent === null ? '—' : `${Math.round(account.remainingPercent)}%` }}</span>
      <span class="mt-1 block text-xs" :class="running ? 'font-semibold text-accent' : 'text-muted'">{{ running ? `${running} running` : windows.length ? '' : note }}</span>
    </span>
  </button>
</template>
