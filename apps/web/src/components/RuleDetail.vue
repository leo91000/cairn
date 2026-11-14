<script setup lang="ts">
import type { HookRule } from '../../../../packages/contracts/generated/typescript/HookRule'
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleVersion } from '../../../../packages/contracts/generated/typescript/HookRuleVersion'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { deleteHookRule } from '../../../../packages/contracts/generated/deleteHookRule'
import { duplicateHookRule } from '../../../../packages/contracts/generated/duplicateHookRule'
import { liftHookRuleBudget } from '../../../../packages/contracts/generated/liftHookRuleBudget'
import { listHookRuleVersions } from '../../../../packages/contracts/generated/listHookRuleVersions'
import { setHookRuleEmergencyPause } from '../../../../packages/contracts/generated/setHookRuleEmergencyPause'
import { setHookRuleEnabled } from '../../../../packages/contracts/generated/setHookRuleEnabled'
import { api, notify, state } from '../api'
import {
  blockLifters,
  blockTitles,
  describeTrigger,
  relativeTime,
} from '../hook-rules'
import {
  CirclePause,
  Copy,
  History,
  Pencil,
  Play,
  ShieldAlert,
  Trash2,
  Webhook,
  X,
  Zap,
} from '../icons'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import RuleFlow from './RuleFlow.vue'
import RuleHistory from './RuleHistory.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

// One rule: what blocks it and who can lift that, what it does, and what it did.
const props = defineProps<{
  rule: HookRule
  catalog: HookRuleCatalog | null
  sheet?: boolean
}>()
const emit = defineEmits<{ changed: [rule: HookRule | null], close: [] }>()
const router = useRouter()

const tab = ref<'flow' | 'history' | 'versions'>('flow')
const busy = ref(false)
const error = ref('')
const deleting = ref(false)
const versions = ref<HookRuleVersion[] | null>(null)
let versionsGeneration = 0
let disposed = false
let duplicateId = crypto.randomUUID()
onBeforeUnmount(() => {
  disposed = true
  versionsGeneration++
})

const agentName = (id: string) => props.catalog?.agents.find(agent => agent.agentId === id)?.name ?? state.agents.find(agent => agent.id === id)?.name ?? 'Unknown agent'
const trigger = computed(() => describeTrigger(props.rule.definition.trigger, props.catalog))
const creator = computed(() => props.rule.creatorAccountId === state.accountId ? 'you' : state.installationRole === 'owner' ? 'a member' : 'another person')
const active = computed(() => props.rule.blocks.length === 0)

async function act(work: () => Promise<HookRule | null>, done: string) {
  busy.value = true
  error.value = ''
  try {
    const next = await work()
    notify(done)
    emit('changed', next)
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

const toggle = () => act(() => setHookRuleEnabled(api, { ruleId: props.rule.id, enabled: !!props.rule.blocks.some(block => block.kind === 'disabled') }), props.rule.blocks.some(block => block.kind === 'disabled') ? 'Rule enabled' : 'Rule disabled')
const pause = (paused: boolean) => act(() => setHookRuleEmergencyPause(api, { ruleId: props.rule.id, paused }), paused ? 'Rule paused' : 'Rule resumed')
const lift = () => act(() => liftHookRuleBudget(api, { ruleId: props.rule.id }), 'Rule resumed')

async function duplicate() {
  await act(async () => {
    const copy = await duplicateHookRule(api, { ruleId: props.rule.id, creationId: duplicateId })
    duplicateId = crypto.randomUUID()
    void router.replace({ query: { rule: copy.id } })
    return copy
  }, 'Duplicated: the copy is yours and starts disabled')
}

async function remove() {
  deleting.value = false
  await act(async () => {
    await deleteHookRule(api, { ruleId: props.rule.id })
    return null
  }, 'Rule deleted')
}

watch(() => [props.rule.id, tab.value], async () => {
  if (tab.value !== 'versions')
    return
  versions.value = null
  const generation = ++versionsGeneration
  try {
    const output = await listHookRuleVersions(api, { ruleId: props.rule.id })
    if (!disposed && generation === versionsGeneration)
      versions.value = output.versions
  }
  catch (e) {
    error.value = (e as Error).message
  }
}, { immediate: true })

const sources: Record<string, string> = {
  app: 'from the apps',
  publicMcp: 'through the MCP',
  installationAgent: 'by an agent',
  duplicate: 'as a duplicate',
  template: 'from a template',
}
</script>

<template>
  <article class="rule-detail grid content-start gap-5" :aria-label="rule.definition.name">
    <header class="flex items-start gap-3">
      <div class="min-w-0 flex-1">
        <div class="flex flex-wrap items-center gap-2">
          <span class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-bold" :class="active ? 'bg-success/12 text-success' : 'bg-coral/12 text-coral'">
            <span class="size-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-coral'" />{{ active ? 'Active' : blockTitles[rule.blocks[0].kind] }}
          </span>
          <span class="text-xs text-muted">Version {{ rule.version }} · by {{ creator }} · {{ relativeTime(rule.updatedAt) }}</span>
        </div>
        <h2 class="m-0! mt-2! font-heading text-[26px] leading-8 font-bold tracking-[-0.5px]">
          {{ rule.definition.name }}
        </h2>
        <p v-if="rule.definition.description" class="m-0! mt-1! text-sm text-muted">
          {{ rule.definition.description }}
        </p>
      </div>
      <button
        v-if="sheet"
        type="button"
        class="grid size-9 place-items-center rounded-full border border-line hover:bg-hover"
        aria-label="Close"
        @click="emit('close')"
      >
        <Icon :name="X" :size="16" />
      </button>
    </header>

    <div class="grid grid-cols-2 gap-2.5 phone:grid-cols-1">
      <div class="flex items-center gap-3 rounded-2xl border border-line bg-raised p-3">
        <span class="grid size-9 shrink-0 place-items-center rounded-xl bg-ink text-canvas"><Icon :name="Zap" :size="17" /></span>
        <span class="min-w-0">
          <span class="block text-2xs font-bold tracking-wide text-muted uppercase">When</span>
          <span class="block truncate text-sm font-semibold">{{ trigger.source }} · {{ trigger.title }}</span>
        </span>
        <Icon :name="Webhook" :size="15" class="ml-auto text-muted" />
      </div>
      <div class="flex items-center gap-3 rounded-2xl border border-line bg-raised p-3">
        <AgentAvatar :name="agentName(rule.definition.executionAgentId)" :identity="rule.definition.executionAgentId" :size="36" />
        <span class="min-w-0">
          <span class="block text-2xs font-bold tracking-wide text-muted uppercase">With the rights of</span>
          <span class="block truncate text-sm font-semibold">{{ agentName(rule.definition.executionAgentId) }}</span>
        </span>
      </div>
    </div>

    <div
      v-for="block in rule.blocks"
      :key="block.kind"
      class="flex items-start gap-3 rounded-2xl border border-coral/30 bg-coral/6 p-3.5"
      role="status"
    >
      <Icon :name="ShieldAlert" :size="18" class="mt-0.5 shrink-0 text-coral" />
      <div class="min-w-0 flex-1">
        <p class="m-0! text-sm font-semibold">
          {{ blockTitles[block.kind] }}
        </p>
        <p class="m-0! text-sm text-muted">
          {{ block.reason }} {{ block.reason.includes('can') || block.reason.includes('Duplicate') ? '' : blockLifters[block.kind] }}
        </p>
      </div>
      <UiButton
        v-if="block.kind === 'disabled' && rule.allowed.enable"
        size="small"
        :disabled="busy"
        @click="toggle"
      >
        Enable
      </UiButton>
      <UiButton
        v-else-if="block.kind === 'emergencyPaused' && rule.allowed.emergencyPause"
        size="small"
        :disabled="busy"
        @click="pause(false)"
      >
        Resume
      </UiButton>
      <UiButton
        v-else-if="block.kind === 'budgetExhausted' && rule.allowed.liftBudget"
        size="small"
        :disabled="busy"
        @click="lift"
      >
        Resume
      </UiButton>
      <UiButton v-else-if="block.kind === 'rightsLost' && rule.allowed.edit" size="small" @click="router.push(`/rules/${rule.id}/edit`)">
        Edit
      </UiButton>
    </div>

    <p
      v-for="warning in rule.warnings"
      :key="warning.message"
      class="m-0! flex items-start gap-2.5 rounded-2xl border border-[#e7b84d]/50 bg-[#fff7e0] p-3.5 text-sm text-[#7a5600] dark:bg-[#3a2e10] dark:text-[#ffd57a]"
      role="note"
    >
      <Icon :name="ShieldAlert" :size="18" class="mt-px shrink-0" />{{ warning.message }}
    </p>

    <div class="flex flex-wrap gap-2">
      <UiButton v-if="rule.allowed.edit" variant="primary" @click="router.push(`/rules/${rule.id}/edit`)">
        <Icon :name="Pencil" :size="15" />Edit
      </UiButton>
      <UiButton v-if="rule.allowed.disable" :disabled="busy" @click="toggle">
        <Icon :name="CirclePause" :size="15" />Disable
      </UiButton>
      <UiButton v-else-if="rule.allowed.enable && !rule.blocks.some(block => block.kind === 'disabled')" :disabled="busy" @click="toggle">
        <Icon :name="Play" :size="15" />Enable
      </UiButton>
      <UiButton v-if="rule.allowed.duplicate" :disabled="busy" @click="duplicate">
        <Icon :name="Copy" :size="15" />Duplicate
      </UiButton>
      <UiButton
        v-if="rule.allowed.emergencyPause && !rule.blocks.some(block => block.kind === 'emergencyPaused')"
        :disabled="busy"
        title="Stops it and its triggerings in progress; only you can resume it."
        @click="pause(true)"
      >
        <Icon :name="CirclePause" :size="15" />Emergency pause
      </UiButton>
      <UiButton
        v-if="rule.allowed.delete"
        variant="danger-outline"
        :disabled="busy"
        @click="deleting = true"
      >
        <Icon :name="Trash2" :size="15" />Delete
      </UiButton>
    </div>
    <p v-if="error" class="m-0! text-sm text-coral" role="alert">
      {{ error }}
    </p>

    <UiSegments
      v-model="tab"
      label="Rule sections"
      :options="[{ value: 'history', label: 'History', icon: History }, { value: 'flow', label: 'Flow' }, { value: 'versions', label: 'Versions' }]"
    />
    <RuleHistory
      v-if="tab === 'history'"
      :rule-id="rule.id"
      :version="rule.version"
      :definition="rule.definition"
      :catalog="catalog"
      :agent-name="agentName"
    />
    <RuleFlow
      v-else-if="tab === 'flow'"
      :nodes="rule.definition.steps"
      container="/steps"
      :catalog="catalog"
      :agent-name="agentName"
    />
    <section v-else class="grid gap-2" aria-label="Versions">
      <p v-if="!versions" class="m-0! text-sm text-muted">
        Loading…
      </p>
      <article v-for="version in versions ?? []" :key="version.version" class="rounded-2xl border border-line bg-raised p-3.5">
        <p class="m-0! text-sm font-semibold">
          Version {{ version.version }}<span class="font-normal text-muted"> · {{ new Date(version.savedAt).toLocaleString() }} · saved {{ sources[version.source] }}{{ version.savedByAccountId === state.accountId ? ' by you' : '' }}</span>
        </p>
        <p class="m-0! mt-1! text-xs text-muted">
          {{ version.definition.steps.length }} {{ version.definition.steps.length === 1 ? 'step' : 'steps' }}{{ version.nativeAuthority ? ' · may reach native agents' : '' }}
        </p>
        <RouterLink v-if="version.conversationId" :to="`/chats/${version.conversationId}`" class="mt-2 inline-block text-xs font-semibold text-accent">
          Open the conversation where this version was written
        </RouterLink>
        <details class="mt-3">
          <summary class="cursor-pointer text-xs font-semibold text-muted">
            View this version’s flow
          </summary>
          <div class="mt-3">
            <RuleFlow
              :nodes="version.definition.steps"
              container="/steps"
              :catalog="catalog"
              :agent-name="agentName"
            />
          </div>
        </details>
      </article>
    </section>

    <Modal v-if="deleting" title="Delete this rule?" @close="deleting = false">
      <div class="px-6.5 py-6 phone:p-5">
        <p>“{{ rule.definition.name }}” stops at once, with its triggerings in progress. Its history goes with it.</p>
      </div>
      <footer class="flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton @click="deleting = false">
          Keep the rule
        </UiButton><UiButton variant="danger" @click="remove">
          Delete the rule
        </UiButton>
      </footer>
    </Modal>
  </article>
</template>
