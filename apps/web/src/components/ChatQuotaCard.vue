<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ChatQuota } from '../chat-quota'
import { Clock } from '../icons'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

defineProps<{
  quota: ChatQuota
  canManage: boolean
  busy?: boolean
  choices?: { provider: Provider, label: string }[]
  switching?: string
  switchError?: string
}>()
const emit = defineEmits<{ resume: [], switch: [provider: Provider], cancelSwitch: [] }>()
</script>

<template>
  <section class="chat-quota my-4 rounded-2xl border border-warning/30 bg-warning-surface p-5" aria-label="Coding agent quota reached" role="status">
    <div class="flex items-start gap-3">
      <Icon :name="Clock" :size="22" class="mt-0.5 shrink-0 text-warning" />
      <div class="min-w-0">
        <h2 class="m-0! text-base font-semibold text-warning">
          {{ quota.provider }} quota reached
        </h2>
        <p class="mt-2! mb-0! text-sm text-ink">
          {{ quota.spend ? 'This account has reached its spending limit.' : 'This account has reached its usage limit.' }}
        </p>
      </div>
    </div>
    <p v-if="quota.reset" class="mt-4! mb-0! text-sm text-warning">
      Reset announced: {{ quota.reset }}
    </p>
    <p class="mt-2! mb-0! text-sm text-muted">
      {{ quota.automatic ? 'Work resumes automatically when an account is available.' : 'Resume the conversation when an account is available.' }}
    </p>
    <div class="mt-4 flex flex-wrap items-center gap-3">
      <UiButton
        v-for="choice in choices"
        :key="choice.provider"
        size="small"
        :disabled="busy || !!switching"
        @click="emit('switch', choice.provider)"
      >
        Continue with {{ choice.label }}
      </UiButton>
      <RouterLink v-if="canManage" to="/connections" class="rounded-full border border-warning/40 px-4 py-2.5 text-sm font-semibold text-warning">
        Manage accounts
      </RouterLink>
      <UiButton
        v-if="!quota.automatic"
        size="small"
        :disabled="busy || !!switching"
        @click="emit('resume')"
      >
        Resume conversation
      </UiButton>
    </div>
    <p v-if="switching" class="mt-3! mb-0! text-sm text-warning" role="status">
      Switching to {{ switching }}… Your conversation and files stay here.
    </p>
    <UiButton
      v-if="switching"
      size="small"
      :disabled="busy"
      @click="emit('cancelSwitch')"
    >
      Cancel switch
    </UiButton>
    <p v-if="switchError" class="mt-3! mb-0! text-sm text-warning" role="alert">
      {{ switchError }}
    </p>
    <p v-if="!canManage && !choices?.length && !switching" class="mt-3! mb-0! text-sm text-muted">
      Ask the installation owner to check the accounts.
    </p>
    <details v-if="quota.message" class="mt-3 text-xs text-muted">
      <summary class="cursor-pointer py-2">
        Technical details
      </summary>
      <p class="mt-2! mb-0! whitespace-pre-wrap wrap-anywhere leading-relaxed">
        {{ quota.message }}
      </p>
    </details>
  </section>
</template>
