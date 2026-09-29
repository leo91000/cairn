<script setup lang="ts">
import type { Account, Provider } from '../../shared/accounts'
import { computed, ref } from 'vue'
import { providers } from '../../shared/accounts'
import { useAccounts } from '../accounts'
import AccountDetail from '../components/AccountDetail.vue'
import AccountRow from '../components/AccountRow.vue'
import AccountSignIn from '../components/AccountSignIn.vue'
import GithubConnection from '../components/GithubConnection.vue'
import Icon from '../components/Icon.vue'
import OnePasswordAccounts from '../components/OnePasswordAccounts.vue'
import ProviderMark from '../components/ProviderMark.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import { Plus, RefreshCw } from '../icons'

// The accounts coding agents run on, grouped by coding agent, then the tools agents reach.
const accounts = useAccounts()
const opened = ref<string>()
const adding = ref<Provider | 'any'>()
const detail = computed<Account | undefined>(() => accounts.accounts.value.find(a => a.id === opened.value))
const groups = computed(() => (Object.keys(providers) as Provider[]).map((provider) => {
  const items = accounts.accounts.value.filter(a => a.provider === provider)
  return {
    provider,
    items,
    available: items.filter(a => ['next', 'low', 'ready'].includes(a.status)).length,
    running: items.reduce((total, a) => total + a.activeRunIds.length, 0),
  }
}))
// A sign-in that is still pending or failed reopens its window, including after a reload.
const signingIn = computed(() => adding.value !== undefined || ['pending', 'failed'].includes(accounts.signIn.value?.state ?? ''))

async function reconnect(account: Account) {
  opened.value = undefined
  await accounts.reconnect(account.id)
}
</script>

<template>
  <div class="page-heading mb-7 flex items-end justify-between gap-5 phone:mb-5 phone:items-center">
    <div>
      <h1>Connections</h1>
      <p class="mt-2 text-[13px] text-muted phone:hidden">
        The accounts your agents run on, and the tools they can reach.
      </p>
    </div>
    <UiButton
      variant="primary"
      :disabled="accounts.busy.value"
      aria-label="Add account"
      @click="adding = 'any'"
    >
      <Icon :name="Plus" :size="16" /><span class="phone:hidden">Add account</span>
    </UiButton>
  </div>
  <UiAlert v-if="accounts.error.value && !signingIn && !detail" class="mb-4">
    {{ accounts.error.value }}
  </UiAlert>

  <section aria-labelledby="coding-agents">
    <div class="mb-3 flex items-end justify-between gap-4">
      <h2 id="coding-agents" class="eyebrow">
        Coding agents
      </h2>
      <span class="flex items-center gap-2 text-xs text-subtle">
        <span class="phone:hidden">New runs use the account with the most capacity left</span>
        <button
          type="button"
          class="grid size-7 place-items-center rounded-full text-subtle hover:bg-hover hover:text-ink disabled:opacity-50"
          aria-label="Check usage now"
          title="Check usage now"
          :disabled="accounts.busy.value"
          @click="accounts.refresh()"
        >
          <Icon :name="RefreshCw" :size="14" :class="{ 'animate-spin motion-reduce:animate-none': accounts.busy.value }" />
        </button>
      </span>
    </div>
    <p v-if="!accounts.loaded.value" class="py-6 text-muted" role="status">
      Loading accounts…
    </p>
    <div v-else class="grid gap-3.5">
      <section
        v-for="group in groups"
        :key="group.provider"
        class="account-group overflow-hidden rounded-2xl border border-line bg-surface shadow-arcade"
        :aria-label="`${providers[group.provider].label} accounts`"
      >
        <header class="flex items-center gap-3 px-4.5 py-4 phone:px-4">
          <ProviderMark :provider="group.provider" />
          <div class="min-w-0 flex-1">
            <h3 class="font-heading text-[15px] font-bold">
              {{ providers[group.provider].label }}
            </h3>
            <p class="text-xs text-muted">
              {{ providers[group.provider].vendor }} · {{ group.items.length }} {{ group.items.length === 1 ? 'account' : 'accounts' }}<span v-if="group.running" class="font-semibold text-accent"> · {{ group.running }} running</span>
            </p>
          </div>
          <span v-if="group.items.length" class="text-xs text-muted phone:hidden">{{ group.available }} available</span>
          <UiButton
            size="small"
            class="border-transparent text-muted"
            :disabled="accounts.busy.value"
            :aria-label="`Add ${providers[group.provider].label} account`"
            @click="adding = group.provider"
          >
            <Icon :name="Plus" :size="14" /><span class="phone:hidden">Add</span>
          </UiButton>
        </header>
        <AccountRow
          v-for="account in group.items"
          :key="account.id"
          :account="account"
          :selected="account.id === opened"
          @open="opened = account.id"
        />
        <p v-if="!group.items.length" class="border-t border-line px-4.5 py-4 text-sm text-muted phone:px-4">
          No {{ providers[group.provider].label }} account yet.<template v-if="accounts.required.value.includes(group.provider)">
            {{ providers[group.provider].label }} runs wait until you add one.
          </template>
        </p>
      </section>
    </div>
  </section>

  <section class="mt-8" aria-labelledby="tools">
    <h2 id="tools" class="eyebrow mb-3">
      Tools
    </h2>
    <div class="connection-card overflow-hidden rounded-2xl border border-line bg-surface shadow-arcade">
      <GithubConnection />
      <OnePasswordAccounts />
    </div>
  </section>

  <AccountDetail
    v-if="detail"
    :account="detail"
    :accounts="accounts"
    @close="opened = undefined"
    @reconnect="reconnect(detail)"
  />
  <AccountSignIn
    v-if="signingIn"
    :accounts="accounts"
    :provider="adding === 'any' ? undefined : adding"
    @close="adding = undefined"
  />
</template>
