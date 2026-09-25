<script setup lang="ts">
import type { Account } from '../../shared/accounts'
import type { RunListItem } from '../../shared/contracts'
import type { AccountsState } from '../accounts'
import { computed, ref, watch } from 'vue'
import { generalWindows, providers, remainingPercent } from '../../shared/accounts'
import { resetsIn } from '../accounts'
import { api } from '../api'
import {
  Minus,
  Plus,
  RefreshCw,
  RotateCcw,
  Trash2,
} from '../icons'
import AccountBadge from './AccountBadge.vue'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import ProviderMark from './ProviderMark.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UsageBar from './UsageBar.vue'
import UsageRing from './UsageRing.vue'

// Everything about one account: its usage windows, what runs on it and its settings.
const props = defineProps<{ account: Account, accounts: AccountsState }>()
const emit = defineEmits<{ close: [], reconnect: [] }>()
const name = ref(props.account.name)
const removing = ref(false)
const runs = ref<RunListItem[]>([])
const general = computed(() => generalWindows(props.account.usage))
const others = computed(() => (props.account.usage?.windows ?? []).filter(window => window.models.length))
const paused = computed(() => props.account.status === 'paused')
const busy = computed(() => props.accounts.busy.value)
const running = computed(() => props.account.activeRunIds)
const provider = computed(() => providers[props.account.provider])
watch(() => props.account.name, value => (name.value = value))
watch(() => running.value.join(), async () => {
  if (!running.value.length) {
    runs.value = []
    return
  }

  const active = await api<RunListItem[]>('/runs?status=running&limit=100').catch(() => [])
  runs.value = active.filter(run => running.value.includes(run.id))
}, { immediate: true })

async function rename() {
  if (name.value.trim() && name.value.trim() !== props.account.name)
    await props.accounts.update(props.account.id, { name: name.value.trim() })
}

async function remove() {
  await props.accounts.remove(props.account.id)
  if (!props.accounts.error.value) {
    removing.value = false
    emit('close')
  }
}

const note = computed(() => {
  const { usage, state } = props.account
  if (state === 'pending')
    return 'Finish signing in to see this account’s usage.'
  if (usage?.error)
    return usage.error
  return props.account.status === 'unavailable' ? 'Usage appears after the first check, within a minute.' : `${provider.value.label} has not reported usage yet. Runs can still use this account.`
})
</script>

<template>
  <Modal :title="account.name" drawer @close="emit('close')">
    <template #heading>
      <span class="flex min-w-0 items-center gap-3.5">
        <AgentAvatar
          :name="account.name"
          :identity="account.id"
          :size="44"
          :working="running.length > 0"
        />
        <span class="min-w-0">
          <span class="block truncate font-heading text-[19px] leading-tight font-extrabold tracking-[-0.02em]">{{ account.name }}</span>
          <span class="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs font-normal text-muted">
            <ProviderMark :provider="account.provider" :size="15" />
            <span class="truncate">{{ provider.label }}<template v-if="account.email"> · {{ account.email }}</template><template v-if="account.plan"> · {{ account.plan }}</template></span>
          </span>
          <span v-if="account.status !== 'ready'" class="mt-2 flex"><AccountBadge :account="account" /></span>
        </span>
      </span>
    </template>
    <div class="flex flex-1 flex-col">
      <div v-if="account.state === 'error' || accounts.error.value" class="px-6.5 pt-3 phone:px-5">
        <UiAlert>{{ accounts.error.value || account.error || 'Reconnect this account to keep using it.' }}</UiAlert>
      </div>

      <section class="border-b border-line px-6.5 py-5 phone:px-5" aria-labelledby="account-usage">
        <h3 id="account-usage" class="eyebrow mb-3.5">
          Usage left
        </h3>
        <div v-if="general.length" class="flex items-center gap-5">
          <UsageRing :windows="general" :remaining="account.remainingPercent" :muted="paused || account.stale" />
          <dl class="grid min-w-0 flex-1 gap-2.5 text-xs">
            <div v-for="(window, index) in general" :key="window.id" class="grid grid-cols-[10px_minmax(0,1fr)] gap-x-2">
              <span class="mt-[5px] size-2 rounded-full" :class="index === 0 ? 'bg-accent' : 'bg-accent/50'" />
              <dt class="font-semibold">
                {{ window.label }} · <span class="tabular-nums">{{ Math.round(remainingPercent(window)) }}%</span>
              </dt>
              <dd v-if="window.resetsAt" class="col-start-2 text-muted">
                Resets {{ resetsIn(window.resetsAt) }}
              </dd>
            </div>
            <div v-if="account.usage?.resets?.available" class="grid grid-cols-[10px_minmax(0,1fr)] gap-x-2">
              <Icon :name="RotateCcw" :size="11" class="mt-[3px] text-muted" />
              <dt class="font-semibold">
                {{ account.usage.resets.available }} banked {{ account.usage.resets.available === 1 ? 'reset' : 'resets' }}
              </dt>
              <dd class="col-start-2 text-muted">
                Used automatically at 2% left
              </dd>
            </div>
          </dl>
        </div>
        <p v-else class="text-sm text-muted">
          {{ note }}
        </p>
        <div v-if="others.length" class="mt-4 grid gap-3">
          <div v-for="window in others" :key="window.id" class="grid gap-1.5">
            <div class="flex justify-between gap-3 text-xs">
              <span>{{ window.label }}</span><b class="tabular-nums">{{ Math.round(remainingPercent(window)) }}%</b>
            </div>
            <UsageBar :remaining="remainingPercent(window)" :muted="paused || account.stale" :label="`${account.name} ${window.label} remaining`" />
          </div>
        </div>
        <p v-if="account.resetError" class="mt-3 text-xs text-warning" role="status">
          {{ account.resetError }}
        </p>
        <p v-else-if="general.length && account.stale" class="mt-3 text-xs text-muted" role="status">
          {{ account.usage?.error || 'These values are out of date. Usage is checked again automatically.' }}
        </p>
      </section>

      <section class="border-b border-line px-6.5 py-5 phone:px-5" aria-labelledby="account-runs">
        <h3 id="account-runs" class="eyebrow mb-2">
          Running now · {{ running.length }} of {{ account.maxConcurrentRuns }}
        </h3>
        <p v-if="!running.length" class="text-sm text-muted">
          Nothing is running on this account.
        </p>
        <RouterLink
          v-for="id in running"
          :key="id"
          :to="`/runs/${id}`"
          class="mt-2 flex items-center gap-2.5 rounded-xl bg-inset px-3 py-2.5 text-sm text-ink hover:bg-hover"
          @click="emit('close')"
        >
          <AgentAvatar :name="runs.find(run => run.id === id)?.agentName || 'Agent'" :size="24" working />
          <span class="min-w-0 flex-1 truncate">{{ runs.find(run => run.id === id)?.taskName || 'Run' }}</span>
          <span class="shrink-0 text-2xs text-muted">{{ runs.find(run => run.id === id)?.agentName }}</span>
        </RouterLink>
      </section>

      <section class="grid gap-4 px-6.5 py-5 phone:px-5" aria-label="Account settings">
        <label class="gap-1.5 text-sm text-ink">
          Name
          <input
            v-model="name"
            maxlength="100"
            autocomplete="off"
            :disabled="busy"
            @change="rename"
            @keydown.enter.prevent="rename"
          >
        </label>
        <div class="flex items-center justify-between gap-4 text-sm">
          <span>Parallel runs<small class="block text-2xs text-muted">Lowering it lets current runs finish</small></span>
          <span class="inline-flex h-9 items-center rounded-full border border-line">
            <button
              type="button"
              class="grid size-9 place-items-center rounded-full text-muted hover:text-ink disabled:opacity-40"
              aria-label="Fewer parallel runs"
              :disabled="busy || account.maxConcurrentRuns <= 1"
              @click="accounts.update(account.id, { maxConcurrentRuns: account.maxConcurrentRuns - 1 })"
            >
              <Icon :name="Minus" :size="14" />
            </button>
            <b class="min-w-7 text-center font-heading tabular-nums" aria-live="polite">{{ account.maxConcurrentRuns }}</b>
            <button
              type="button"
              class="grid size-9 place-items-center rounded-full text-muted hover:text-ink disabled:opacity-40"
              aria-label="More parallel runs"
              :disabled="busy"
              @click="accounts.update(account.id, { maxConcurrentRuns: account.maxConcurrentRuns + 1 })"
            >
              <Icon :name="Plus" :size="14" />
            </button>
          </span>
        </div>
        <label class="flex-row items-center justify-between gap-4 text-sm text-ink">
          <span>Use for new runs<small class="block text-2xs text-muted">Turn off to pause this account</small></span>
          <input
            type="checkbox"
            role="switch"
            class="account-switch"
            :checked="account.enabled"
            :disabled="busy || account.state === 'pending'"
            @change="accounts.update(account.id, { enabled: !account.enabled })"
          >
        </label>
      </section>

      <footer class="mt-auto flex flex-wrap items-center gap-2 border-t border-line px-6.5 py-4 phone:px-5">
        <UiButton size="small" :disabled="busy || running.length > 0" @click="emit('reconnect')">
          <Icon :name="RefreshCw" :size="14" />{{ account.state === 'pending' ? 'Sign in' : 'Reconnect' }}
        </UiButton>
        <UiButton
          size="small"
          class="ml-auto border-transparent text-danger"
          :disabled="busy || running.length > 0"
          @click="removing = true"
        >
          <Icon :name="Trash2" :size="14" />Remove
        </UiButton>
      </footer>
    </div>
  </Modal>
  <Modal v-if="removing" :title="`Remove ${account.name}?`" @close="removing = false">
    <div class="grid gap-4 p-6">
      <p>Its saved credentials are deleted from this server. Runs that use {{ provider.label }} will use your other accounts.</p>
      <UiAlert v-if="accounts.error.value">
        {{ accounts.error.value }}
      </UiAlert>
    </div>
    <footer class="flex justify-end gap-3 border-t border-line p-4">
      <UiButton @click="removing = false">
        Cancel
      </UiButton>
      <UiButton variant="danger" :disabled="busy" @click="remove">
        Remove account
      </UiButton>
    </footer>
  </Modal>
</template>

<style scoped>
.account-switch {
  appearance: none;
  position: relative;
  width: 38px;
  height: 22px;
  flex-shrink: 0;
  border-radius: 999px;
  background: var(--color-control);
  transition: background-color 0.18s;
  cursor: pointer;
}
.account-switch::after {
  content: '';
  position: absolute;
  top: 3px;
  left: 3px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 2px #0003;
  transition: transform 0.2s var(--ease-signal);
}
.account-switch:checked { background: var(--color-accent); }
.account-switch:checked::after { transform: translateX(16px); }
.account-switch:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 3px; }
.account-switch:disabled { opacity: 0.5; cursor: not-allowed; }
@media (prefers-reduced-motion: reduce) {
  .account-switch, .account-switch::after { transition: none; }
}
</style>
