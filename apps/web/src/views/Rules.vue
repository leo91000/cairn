<script setup lang="ts">
import type { HookRule } from '../../../../packages/contracts/generated/typescript/HookRule'
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleSummary } from '../../../../packages/contracts/generated/typescript/HookRuleSummary'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { getHookRule } from '../../../../packages/contracts/generated/getHookRule'
import { getHookRuleCatalog } from '../../../../packages/contracts/generated/getHookRuleCatalog'
import { listHookRules } from '../../../../packages/contracts/generated/listHookRules'
import { api, state } from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Empty from '../components/Empty.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import RuleDetail from '../components/RuleDetail.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import { blockTitles, describeTrigger, relativeTime } from '../hook-rules'
import { Plus, Search, Zap } from '../icons'

// Rules as missions are shown: cards on the left, one rule beside them on wide screens or
// in a sheet on phones. Every member sees every rule and its state.
const route = useRoute()
const router = useRouter()
const rules = ref<HookRuleSummary[]>([])
const catalog = ref<HookRuleCatalog | null>(null)
const shownRule = ref<HookRule | null>(null)
const loading = ref(true)
const error = ref('')
const catalogError = ref('')
const catalogLoading = ref(false)
const query = ref('')
const filter = ref<'all' | 'mine' | 'blocked'>('all')
const wideQuery = window.matchMedia('(width > 900px)')
const wide = ref(wideQuery.matches)
const onWide = () => wide.value = wideQuery.matches
let timer: ReturnType<typeof setInterval>
let disposed = false
let listRequest = 0
let detailRequest = 0

const listInput = computed(() => ({
  ...(typeof route.query.agent === 'string' ? { agentId: route.query.agent } : {}),
  ...(typeof route.query.instance === 'string' ? { instanceId: route.query.instance } : {}),
}))
const listScope = computed(() => JSON.stringify([state.installationId, state.accountId, listInput.value]))
const scoped = computed(() => rules.value)
const filters = computed(() => [
  { value: 'all' as const, label: 'All', count: scoped.value.length },
  { value: 'mine' as const, label: 'Mine', count: scoped.value.filter(rule => rule.creatorAccountId === state.accountId).length },
  { value: 'blocked' as const, label: 'Stopped', count: scoped.value.filter(rule => rule.blocks.length).length },
])
const visible = computed(() => scoped.value.filter(rule =>
  (filter.value === 'all' || (filter.value === 'mine' ? rule.creatorAccountId === state.accountId : rule.blocks.length > 0))
  && `${rule.name} ${rule.description}`.toLowerCase().includes(query.value.trim().toLowerCase())))
const creatorNames = ref<Record<string, string>>({})
const creatorName = (account: string) => account === state.accountId ? 'you' : creatorNames.value[account] ?? 'another person'
const newRuleLocation = computed(() => ({ path: '/rules/new', query: { ...(typeof route.query.agent === 'string' ? { agent: route.query.agent } : {}), ...(typeof route.query.instance === 'string' ? { instance: route.query.instance } : {}) } }))

function clearScope() {
  const { agent: _agent, instance: _instance, ...query } = route.query
  void router.replace({ query })
}

const chosenId = computed(() => typeof route.query.rule === 'string' ? route.query.rule : null)
const shownId = computed(() => chosenId.value ?? (wide.value ? visible.value[0]?.id : undefined))
const summary = computed(() => {
  const active = scoped.value.filter(rule => !rule.blocks.length).length
  return `${active} active · ${scoped.value.length - active} stopped`
})
const agentName = (id: string) => state.agents.find(agent => agent.id === id)?.name ?? catalog.value?.agents.find(agent => agent.agentId === id)?.name ?? 'Agent'

async function load() {
  const request = ++listRequest
  const scope = listScope.value
  const current = () => !disposed && request === listRequest && scope === listScope.value
  try {
    const output = await listHookRules(api, listInput.value)
    if (!current())
      return
    rules.value = output.rules
    error.value = ''
  }
  catch (e) {
    if (current())
      error.value = (e as Error).message
  }
  finally {
    if (current())
      loading.value = false
  }
}

async function loadShown() {
  const request = ++detailRequest
  const scope = listScope.value
  const id = shownId.value
  const current = () => !disposed && request === detailRequest && scope === listScope.value && shownId.value === id
  if (!id) {
    shownRule.value = null
    return
  }

  try {
    const rule = await getHookRule(api, { ruleId: id })
    if (current())
      shownRule.value = rule
  }
  catch (e) {
    if (current())
      error.value = (e as Error).message
  }
}

function select(rule: HookRuleSummary) {
  void router.replace({ query: { ...route.query, rule: rule.id } })
}

function closeSheet() {
  const { rule: _rule, ...rest } = route.query
  void router.replace({ query: rest })
}

async function changed(rule: HookRule | null) {
  // Responses started before a confirmed mutation must never restore its old state.
  listRequest++
  detailRequest++
  if (!rule) {
    rules.value = rules.value.filter(entry => entry.id !== shownId.value)
    shownRule.value = null
    closeSheet()
  }
  else {
    shownRule.value = rule
    rules.value = rules.value.map(entry => entry.id === rule.id
      ? {
          ...entry,
          revision: rule.revision,
          version: rule.version,
          blocks: rule.blocks,
          allowed: rule.allowed,
          updatedAt: rule.updatedAt,
        }
      : entry)
  }

  await load()
}

watch(listScope, () => {
  listRequest++
  detailRequest++
  rules.value = []
  shownRule.value = null
  error.value = ''
  loading.value = true
  void load()
}, { immediate: true, flush: 'sync' })

watch([shownId, listScope], () => {
  shownRule.value = null
  void loadShown()
}, { immediate: true })

async function loadCatalog() {
  catalogLoading.value = true
  catalogError.value = ''
  try {
    const value = await getHookRuleCatalog(api, {})
    if (!disposed)
      catalog.value = value
  }
  catch (e) {
    if (!disposed)
      catalogError.value = (e as Error).message || 'The rule catalog could not be loaded.'
  }
  finally {
    if (!disposed)
      catalogLoading.value = false
  }
}

onMounted(() => {
  wideQuery.addEventListener('change', onWide)
  const accountScope = `${state.installationId}:${state.accountId}`
  if (state.installationRole === 'owner') {
    void fetch(`/api/installations/${encodeURIComponent(state.installationId)}/sharing`, { headers: { 'X-CSRF-Token': state.csrf } })
      .then(async response => response.ok ? response.json() : null)
      .then((sharing) => {
        if (!disposed && state.installationRole === 'owner' && accountScope === `${state.installationId}:${state.accountId}` && sharing)
          creatorNames.value = Object.fromEntries((sharing.members ?? []).map((member: { id: string, email: string }) => [member.id, member.email]))
      })
      .catch(() => {})
  }

  void loadCatalog()
  timer = setInterval(() => {
    if (!document.hidden) {
      void load()
      void loadShown()
    }
  }, 10000)
})
onBeforeUnmount(() => {
  disposed = true
  clearInterval(timer)
  wideQuery.removeEventListener('change', onWide)
})
</script>

<template>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <div class="rules flex min-h-0 flex-1">
    <section class="flex min-h-0 w-111 shrink-0 flex-col pr-6 tablet:w-full tablet:pr-0" aria-label="Rule list">
      <header class="flex items-start gap-2 pr-1">
        <div class="min-w-0 flex-1">
          <h1 class="m-0! font-heading text-[32px] leading-9 font-bold tracking-[-0.8px]">
            Rules
          </h1>
          <p class="m-0! mt-0.5! truncate text-base text-muted">
            {{ loading ? 'When this happens, do that' : summary }}
          </p>
        </div>
        <button
          class="grid size-11 shrink-0 place-items-center rounded-full bg-ink text-canvas hover:opacity-90"
          aria-label="New rule"
          :disabled="!catalog?.agents.length"
          @click="router.push(newRuleLocation)"
        >
          <Icon :name="Plus" :size="20" />
        </button>
      </header>
      <div v-if="catalogError" class="mt-3 rounded-xl bg-coral/8 p-3 text-sm" role="alert">
        <p class="m-0!">
          The rule catalog could not be loaded. {{ catalogError }}
        </p>
        <button
          type="button"
          class="mt-2 font-semibold text-accent"
          :disabled="catalogLoading"
          @click="loadCatalog"
        >
          Retry loading the catalog
        </button>
      </div>
      <div v-if="route.query.agent || route.query.instance" class="mt-3 flex flex-wrap items-center gap-2 rounded-xl bg-accent/8 px-3 py-2 text-xs" role="status">
        <span v-if="typeof route.query.agent === 'string'">Agent: {{ agentName(route.query.agent) }}</span>
        <span v-if="typeof route.query.instance === 'string'">Plugin: {{ catalog?.hooks.find(entry => entry.trigger.type === 'plugin' && entry.trigger.instanceId === route.query.instance)?.instanceName ?? 'selected instance' }}</span>
        <button type="button" class="ml-auto font-semibold text-accent" @click="clearScope">
          Show all rules
        </button>
      </div>
      <label class="search-field mt-3 flex flex-row! min-h-11 items-center gap-2 rounded-full border border-line bg-surface px-4">
        <Icon :name="Search" :size="16" class="text-muted" /><input
          v-model="query"
          class="w-full! min-w-0 flex-1 outline-none"
          placeholder="Search rules"
          aria-label="Search rules"
        >
      </label>
      <div class="mt-2 flex shrink-0 gap-2 py-1.5" role="group" aria-label="Filter rules">
        <button
          v-for="option in filters"
          :key="option.value"
          class="flex h-9 shrink-0 items-center gap-1.75 rounded-full border px-3.5 text-sm font-semibold"
          :class="filter === option.value ? 'border-ink bg-ink text-canvas' : 'border-line text-ink hover:bg-hover'"
          :aria-pressed="filter === option.value"
          @click="filter = option.value"
        >
          {{ option.label }}<span :class="filter === option.value ? 'text-canvas/60' : 'text-muted'">{{ option.count }}</span>
        </button>
      </div>
      <ul class="m-0! mt-2! flex min-h-0 flex-1 list-none flex-col gap-2.5 overflow-y-auto overscroll-contain p-0! pb-6! [scrollbar-width:thin]" data-testid="rules">
        <li v-for="rule in visible" :key="rule.id">
          <article class="relative rounded-[22px] border py-3.5 pr-3.5 pl-4" :class="shownId === rule.id && wide ? 'border-accent/30 bg-soft' : rule.blocks.length ? 'border-line bg-transparent' : 'border-line bg-surface'">
            <button class="absolute inset-0 rounded-[22px] focus-visible:outline-2 focus-visible:outline-accent" :aria-label="`Open ${rule.name}`" @click="select(rule)" />
            <div class="pointer-events-none">
              <div class="flex items-center gap-2">
                <span class="inline-flex min-w-0 items-center gap-1.5 rounded-full bg-hover py-0.5 pr-2.5 pl-1.5 text-xs font-semibold text-muted">
                  <span class="grid size-5 place-items-center rounded-full bg-ink text-canvas"><Icon :name="Zap" :size="11" /></span>
                  <span class="truncate">{{ describeTrigger(rule.trigger, catalog).source }} · {{ describeTrigger(rule.trigger, catalog).title }}</span>
                </span>
                <span class="flex-1" />
                <AgentAvatar :name="agentName(rule.executionAgentId)" :identity="rule.executionAgentId" :size="22" />
              </div>
              <p class="m-0! mt-2! line-clamp-2 font-heading text-base font-semibold" :class="rule.blocks.length ? 'text-muted' : 'text-ink'">
                {{ rule.name }}
              </p>
              <p class="m-0! mt-1! flex items-center gap-1.5 text-xs text-muted">
                <span class="size-1.5 rounded-full" :class="rule.blocks.length ? 'bg-coral' : 'bg-success'" />
                <span :class="rule.blocks.length ? 'font-semibold text-coral' : ''">{{ rule.blocks.length ? blockTitles[rule.blocks[0].kind] : 'Active' }}</span>
                <span>· v{{ rule.version }} · {{ creatorName(rule.creatorAccountId) }} · {{ relativeTime(rule.updatedAt) }}</span>
              </p>
            </div>
          </article>
        </li>
        <li v-if="!loading && !visible.length">
          <Empty :title="rules.length ? 'No matching rules' : 'No rules yet'" :description="rules.length ? 'Change the search or the filters.' : 'A rule answers an event without code: when a message arrives, call a tool or start an agent.'">
            <UiButton v-if="!rules.length" :disabled="!catalog?.agents.length" @click="router.push(newRuleLocation)">
              <Icon :name="Plus" :size="16" />Create a rule
            </UiButton>
          </Empty>
        </li>
      </ul>
    </section>
    <section v-if="wide" class="min-h-0 min-w-0 flex-1 overflow-y-auto border-l border-line pl-6 [scrollbar-width:thin]" aria-label="Selected rule">
      <RuleDetail
        v-if="shownRule"
        :key="shownRule.id"
        class="max-w-200"
        :rule="shownRule"
        :catalog="catalog"
        :creator-name="creatorName(shownRule.creatorAccountId)"
        @changed="changed"
      />
      <Empty v-else-if="!loading" title="Your rules, in one place" description="Create a rule to answer events from your plugins." />
    </section>
  </div>
  <Modal
    v-if="!wide && chosenId && shownRule"
    :title="shownRule.definition.name"
    sheet
    headless
    @close="closeSheet"
  >
    <div class="px-5 pt-5 pb-6">
      <RuleDetail
        :key="shownRule.id"
        :rule="shownRule"
        :catalog="catalog"
        :creator-name="creatorName(shownRule.creatorAccountId)"
        sheet
        @changed="changed"
        @close="closeSheet"
      />
    </div>
  </Modal>
</template>
