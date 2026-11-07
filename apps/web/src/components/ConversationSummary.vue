<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ContextSummary } from '../context-usage'
import { providers } from '../../../../packages/contracts/accounts'
import AgentAvatar from './AgentAvatar.vue'

// The top of the conversation menu: who works here and how full its context is, which the
// header leaves out until something needs a look.
defineProps<{
  agentName: string
  agentId?: string
  provider?: Provider | null
  projectName?: string | null
  status?: string
  failed?: boolean
  context?: ContextSummary
}>()
</script>

<template>
  <div>
    <div class="flex items-center gap-2.5 px-2.5 pt-2 pb-3">
      <AgentAvatar
        :name="agentName"
        :identity="agentId"
        :size="28"
        :harness="provider"
      />
      <div class="min-w-0 text-xs leading-snug">
        <p class="m-0! truncate font-semibold text-ink">
          {{ agentName }}<template v-if="status">
            <span aria-hidden="true" class="text-muted"> · </span><span :class="failed ? 'text-coral' : 'font-normal text-accent'">{{ status }}</span>
          </template>
        </p>
        <p class="m-0! truncate text-muted">
          {{ [provider && providers[provider].label, projectName ? `Project ${projectName}` : 'No project'].filter(Boolean).join(' · ') }}
        </p>
      </div>
    </div>
    <div v-if="context" class="mx-2.5 mb-1 border-y border-line py-3 text-xs" data-testid="context-usage">
      <div class="flex items-baseline justify-between gap-3">
        <span>Context</span><span class="tabular-nums text-muted">{{ context.short }}</span>
      </div>
      <div v-if="context.percent !== null" class="mt-2 h-1 overflow-hidden rounded-full bg-inset" aria-hidden="true">
        <span class="block h-full rounded-full" :class="context.percent >= 85 ? 'bg-coral' : 'bg-accent'" :style="{ width: `${context.percent}%` }" />
      </div>
      <p class="m-0! mt-2! text-2xs leading-relaxed text-muted">
        {{ [context.label, ...context.details].join(' · ') }}
      </p>
    </div>
  </div>
</template>
