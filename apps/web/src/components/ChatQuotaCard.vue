<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ChatQuota } from '../chat-quota'
import { LoaderCircle } from '../icons'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

defineProps<{
  quota: ChatQuota
  canManage: boolean
  // Someone who cannot use the conversation's agent sees why it stopped, without its actions.
  readOnly?: boolean
  busy?: boolean
  loading?: boolean
  choices?: { provider: Provider, label: string }[]
  switching?: string
  switchError?: string
}>()
const emit = defineEmits<{ resume: [], switch: [provider: Provider], cancelSwitch: [] }>()
</script>

<template>
  <section class="chat-quota my-4 rounded-xl border border-line bg-surface px-4 py-3" aria-label="Coding agent quota reached" role="status">
    <div class="flex flex-wrap items-center gap-x-4 gap-y-3">
      <div class="flex min-w-0 flex-1 items-start gap-3">
        <Icon
          v-if="switching"
          :name="LoaderCircle"
          :size="14"
          class="mt-1 shrink-0 animate-spin text-accent motion-reduce:animate-none"
        />
        <span v-else class="mt-1.5 size-1.5 shrink-0 rounded-full bg-warning" aria-hidden="true" />
        <div class="min-w-0">
          <h2 class="m-0! text-sm font-semibold text-ink">
            {{ switching ? `Switching to ${switching}…` : `${quota.provider} ${quota.spend ? 'spending limit' : 'quota'} reached` }}
          </h2>
          <p class="mt-1! mb-0! text-xs text-muted">
            {{ switching ? 'Your conversation and files stay here.' : quota.automatic ? 'Work resumes automatically when an account is available.' : 'Resume the conversation when an account is available.' }}
          </p>
          <p v-if="quota.reset && !switching" class="mt-1! mb-0! text-xs text-muted">
            Reset announced: {{ quota.reset }}
          </p>
        </div>
      </div>
      <div v-if="!readOnly && !switching" class="flex flex-wrap items-center gap-2 phone:ml-[18px] phone:w-full">
        <UiButton
          v-for="choice in choices"
          :key="choice.provider"
          size="small"
          class="border-accent/30 bg-soft text-accent enabled:hover:bg-hover"
          :disabled="busy"
          @click="emit('switch', choice.provider)"
        >
          Continue with {{ choice.label }}
        </UiButton>
        <span v-if="loading && !choices?.length" class="inline-flex items-center gap-2 text-xs text-muted">
          <Icon :name="LoaderCircle" :size="14" class="animate-spin motion-reduce:animate-none" />
          Checking alternatives…
        </span>
        <UiButton
          v-if="!quota.automatic"
          size="small"
          :disabled="busy"
          @click="emit('resume')"
        >
          Resume conversation
        </UiButton>
      </div>
      <UiButton
        v-else-if="!readOnly"
        size="small"
        :disabled="busy"
        @click="emit('cancelSwitch')"
      >
        Cancel switch
      </UiButton>
    </div>
    <div v-if="canManage || quota.message" class="mt-1 ml-[18px] flex flex-wrap items-baseline gap-x-4 text-xs text-muted">
      <RouterLink v-if="canManage" to="/connections" class="py-1 underline underline-offset-3 hover:text-ink phone:py-3">
        Manage accounts
      </RouterLink>
      <details v-if="quota.message" class="min-w-0 flex-1">
        <summary class="w-fit cursor-pointer py-1 phone:py-3">
          Technical details
        </summary>
        <p class="mt-2! mb-0! whitespace-pre-wrap wrap-anywhere leading-relaxed">
          {{ quota.message }}
        </p>
      </details>
    </div>
    <p v-if="switchError" class="mt-2! mb-0! ml-[18px] text-xs text-warning" role="alert">
      {{ switchError }}
    </p>
    <p v-if="!readOnly && !canManage && !choices?.length && !switching && !loading && !switchError" class="mt-2! mb-0! ml-[18px] text-xs text-muted">
      Ask the installation owner to check the accounts.
    </p>
  </section>
</template>
