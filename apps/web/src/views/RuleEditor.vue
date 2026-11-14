<script setup lang="ts">
import type { HookRule } from '../../../../packages/contracts/generated/typescript/HookRule'
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleDefinition } from '../../../../packages/contracts/generated/typescript/HookRuleDefinition'
import type { HookRuleProblem } from '../../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { HookRuleTrigger } from '../../../../packages/contracts/generated/typescript/HookRuleTrigger'
import type { HookRuleWarning } from '../../../../packages/contracts/generated/typescript/HookRuleWarning'
import type { RuleNode } from '../../../../packages/contracts/generated/typescript/RuleNode'
import type { StepKind } from '../hook-rules'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { onBeforeRouteLeave, useRoute, useRouter } from 'vue-router'
import { createHookRule } from '../../../../packages/contracts/generated/createHookRule'
import { getHookRule } from '../../../../packages/contracts/generated/getHookRule'
import { getHookRuleCatalog } from '../../../../packages/contracts/generated/getHookRuleCatalog'
import { replaceHookRule } from '../../../../packages/contracts/generated/replaceHookRule'
import { validateHookRule } from '../../../../packages/contracts/generated/validateHookRule'
import { api, ApiError, notify } from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import RuleFlow from '../components/RuleFlow.vue'
import RuleStepEditor from '../components/RuleStepEditor.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  catalogHook,
  emptyDefinition,
  newStep,
  nodeAt,
  pathOfNode,
  problemsUnder,
  schemaFields,
  triggerKey,
  variablesAt,
  withInserted,
  withMoved,
  withRemoved,
  withReplaced,
} from '../hook-rules'
import {
  ArrowLeft,
  Bot,
  CircleAlert,
  CircleCheck,
  LoaderCircle,
  ShieldAlert,
  Webhook,
  Zap,
} from '../icons'

// The visual rule editor: what the rule listens to, with whose rights, and a flow of
// steps edited block by block. The installation checks the rule as it is written.
const route = useRoute()
const router = useRouter()
const id = computed(() => typeof route.params.id === 'string' ? route.params.id : null)
const catalog = ref<HookRuleCatalog | null>(null)
const rule = ref<HookRule | null>(null)
const definition = ref<HookRuleDefinition | null>(null)
const problems = ref<HookRuleProblem[]>([])
const warnings = ref<HookRuleWarning[]>([])
const checking = ref(false)
const saving = ref(false)
const error = ref('')
const selected = ref<string | null>(null)
const wideQuery = window.matchMedia('(width > 1100px)')
const wide = ref(wideQuery.matches)
const onWide = () => wide.value = wideQuery.matches
const creationId = crypto.randomUUID()
const loadedDefinition = ref('')
const validated = ref(false)
let disposed = false
let leavingAfterSave = false
const pendingLeave = ref<string | null>(null)
const conflict = ref<HookRule | null>(null)
const comparingConflict = ref(false)

const agentName = (agent: string) => catalog.value?.agents.find(entry => entry.agentId === agent)?.name ?? 'an agent'

async function load() {
  error.value = ''
  try {
    const [loadedCatalog, loadedRule] = await Promise.all([
      getHookRuleCatalog(api, {}),
      id.value ? getHookRule(api, { ruleId: id.value }) : Promise.resolve(null),
    ])
    catalog.value = loadedCatalog
    rule.value = loadedRule
    if (disposed)
      return
    if (loadedRule && !loadedRule.allowed.edit) {
      await router.replace({ path: '/rules', query: { rule: loadedRule.id } })
      return
    }

    if (loadedRule) {
      definition.value = structuredClone(loadedRule.definition)
    }
    else {
      const hook = loadedCatalog.hooks.find(entry => entry.trigger.type === 'plugin' && entry.trigger.instanceId === route.query.instance) ?? loadedCatalog.hooks.find(entry => entry.trigger.type === 'plugin') ?? loadedCatalog.hooks[0]
      const agent = typeof route.query.agent === 'string' && hook?.agentIds.includes(route.query.agent) ? route.query.agent : hook?.agentIds[0] ?? loadedCatalog.agents[0]?.agentId ?? ''
      definition.value = emptyDefinition(hook?.trigger ?? { type: 'cairn', hook: 'replyReady' }, agent, loadedCatalog.limits.defaultLimits)
    }

    loadedDefinition.value = JSON.stringify(definition.value)
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

// Hooks the person may listen to, grouped by where they come from.
const hookGroups = computed(() => {
  const groups = new Map<string, NonNullable<typeof catalog.value>['hooks']>()
  for (const hook of catalog.value?.hooks ?? []) {
    const group = hook.trigger.type === 'cairn' ? 'Cairn conversations' : hook.instanceName ?? 'Plugins'
    groups.set(group, [...groups.get(group) ?? [], hook])
  }

  return [...groups.entries()]
})
const hook = computed(() => definition.value ? catalogHook(catalog.value, definition.value.trigger) : undefined)
const peopleSource = computed(() => {
  const field = hook.value?.resourceFields.find(entry => /person|user/i.test(entry.resourceType) || /author|sender/i.test(entry.path))
  return field && hook.value?.pluginId && definition.value?.trigger.type === 'plugin'
    ? { pluginId: hook.value.pluginId, instanceId: definition.value.trigger.instanceId, resourceType: field.resourceType }
    : undefined
})
const executionAgents = computed(() => (catalog.value?.agents ?? []).filter(agent => !hook.value || hook.value.agentIds.includes(agent.agentId)))
const payloadFields = computed(() => schemaFields(hook.value?.payloadSchema, 'payload').filter(field => !field.expression.slice(8).includes('.')))

function chooseTrigger(key: string) {
  const next = catalog.value?.hooks.find(entry => triggerKey(entry.trigger) === key)
  if (!next || !definition.value)
    return
  const trigger: HookRuleTrigger = next.trigger
  const agent = next.agentIds.includes(definition.value.executionAgentId) ? definition.value.executionAgentId : next.agentIds[0] ?? definition.value.executionAgentId
  definition.value = { ...definition.value, trigger, executionAgentId: agent }
}

// Validation follows editing, a moment after the last change.
let timer: ReturnType<typeof setTimeout> | undefined
let generation = 0
watch(definition, (value) => {
  if (!value)
    return
  clearTimeout(timer)
  const current = ++generation
  validated.value = false
  checking.value = true
  timer = setTimeout(async () => {
    try {
      const output = await validateHookRule(api, { definition: value, ruleId: id.value })
      if (!disposed && current === generation) {
        validated.value = true
        error.value = ''
        problems.value = output.problems
        warnings.value = output.warnings
      }
    }
    catch (e) {
      if (current === generation)
        error.value = (e as Error).message
    }
    finally {
      if (current === generation)
        checking.value = false
    }
  }, 350)
}, { deep: true })

const general = computed(() => problems.value.filter(problem => !problem.path.startsWith('/steps/')))
const stepProblems = computed(() => problems.value.filter(problem => problem.path.startsWith('/steps')))
const ready = computed(() => validated.value && !checking.value && problems.value.length === 0 && !!definition.value?.name.trim())

function insert(container: string, index: number, kind: StepKind) {
  if (!definition.value)
    return
  const node = newStep(definition.value, kind, catalog.value)
  definition.value = withInserted(definition.value, container, index, node)
  selected.value = `${container}/${index}`
}

function mutateTree(update: (definition: HookRuleDefinition) => HookRuleDefinition) {
  if (!definition.value)
    return
  const selectedId = selected.value ? nodeAt(definition.value, selected.value)?.id : undefined
  definition.value = update(definition.value)
  selected.value = selectedId ? pathOfNode(definition.value, selectedId) : null
}

function remove(path: string) {
  mutateTree(definition => withRemoved(definition, path))
}

function move(path: string, offset: -1 | 1) {
  mutateTree(definition => withMoved(definition, path, offset))
}

const selectedNode = computed({
  get: () => definition.value && selected.value ? nodeAt(definition.value, selected.value) : undefined,
  set: (node: RuleNode | undefined) => {
    if (definition.value && selected.value && node)
      definition.value = withReplaced(definition.value, selected.value, node)
  },
})
const selectedVariables = computed(() => definition.value && selected.value ? variablesAt(definition.value, selected.value, catalog.value) : [])

async function save() {
  if (!definition.value || saving.value)
    return
  saving.value = true
  error.value = ''
  try {
    let saved: HookRule
    if (rule.value) {
      try {
        saved = await replaceHookRule(api, { ruleId: rule.value.id, revision: rule.value.revision, definition: definition.value })
      }
      catch (cause) {
        if (!(cause instanceof ApiError) || cause.status !== 409)
          throw cause
        const latest = await getHookRule(api, { ruleId: rule.value.id })
        if (!latest.allowed.edit)
          throw new Error('You can no longer edit this rule. Your draft is still here.')
        if (latest.version !== rule.value.version) {
          conflict.value = latest
          return
        }

        rule.value = latest
        saved = await replaceHookRule(api, { ruleId: latest.id, revision: latest.revision, definition: definition.value })
      }
    }
    else {
      saved = await createHookRule(api, { creationId, definition: definition.value, enabled: true })
    }

    notify(rule.value ? 'Rule saved as a new version' : 'Rule created')
    leavingAfterSave = true
    await router.push({ path: '/rules', query: { rule: saved.id } })
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    saving.value = false
  }
}

function useLatest() {
  if (!conflict.value)
    return
  rule.value = conflict.value
  definition.value = JSON.parse(JSON.stringify(conflict.value.definition))
  loadedDefinition.value = JSON.stringify(definition.value)
  conflict.value = null
  selected.value = null
}

async function saveDraftOverLatest() {
  if (!conflict.value)
    return
  rule.value = conflict.value
  conflict.value = null
  await save()
}

onMounted(() => {
  wideQuery.addEventListener('change', onWide)
  window.addEventListener('beforeunload', beforeUnload)
  void load()
})
onBeforeRouteLeave((to) => {
  if (leavingAfterSave || !definition.value || JSON.stringify(definition.value) === loadedDefinition.value)
    return true
  pendingLeave.value = to.fullPath
  return false
})

function confirmLeave() {
  const target = pendingLeave.value
  pendingLeave.value = null
  leavingAfterSave = true
  if (target)
    void router.push(target)
}

function beforeUnload(event: BeforeUnloadEvent) {
  if (!leavingAfterSave && definition.value && JSON.stringify(definition.value) !== loadedDefinition.value)
    event.preventDefault()
}

onBeforeUnmount(() => {
  disposed = true
  generation++
  clearTimeout(timer)
  wideQuery.removeEventListener('change', onWide)
  window.removeEventListener('beforeunload', beforeUnload)
})
</script>

<template>
  <div class="rule-editor flex min-h-0 flex-1 flex-col">
    <header class="flex flex-wrap items-center gap-3 border-b border-line pb-4">
      <RouterLink :to="rule ? { path: '/rules', query: { rule: rule.id } } : '/rules'" class="grid size-10 place-items-center rounded-full border border-line text-ink hover:bg-hover" aria-label="Back to the rules">
        <Icon :name="ArrowLeft" :size="18" />
      </RouterLink>
      <div class="min-w-0 flex-1">
        <p class="m-0! text-xs font-semibold tracking-wide text-muted uppercase">
          {{ rule ? `Editing version ${rule.version}` : 'New rule' }}
        </p>
        <input
          v-if="definition"
          v-model="definition.name"
          class="name-input w-full! border-0! bg-transparent! p-0! font-heading text-[26px] font-bold tracking-[-0.5px] shadow-none! outline-none"
          maxlength="100"
          placeholder="Name your rule"
          aria-label="Rule name"
        >
      </div>
      <span class="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-xs font-semibold" :class="checking ? 'bg-hover text-muted' : problems.length ? 'bg-coral/12 text-coral' : 'bg-success/12 text-success'" role="status">
        <Icon :name="checking ? LoaderCircle : problems.length ? CircleAlert : CircleCheck" :size="14" :class="checking ? 'animate-spin' : ''" />
        {{ checking ? 'Checking…' : problems.length ? `${problems.length} to fix` : 'Ready' }}
      </span>
      <UiButton variant="primary" :disabled="!ready || saving || !!conflict" @click="save">
        {{ saving ? 'Saving…' : rule ? 'Save version' : 'Create rule' }}
      </UiButton>
    </header>
    <UiAlert v-if="error" class="mt-3">
      {{ error }}
    </UiAlert>
    <section v-if="conflict" class="mt-3 grid max-h-[35vh] shrink-0 gap-3 overflow-y-auto rounded-2xl border border-accent/30 bg-raised p-4" role="alert">
      <p class="m-0! text-sm">
        Version {{ conflict.version }} was saved while you were editing. Your draft is preserved.
      </p>
      <div class="flex flex-wrap gap-2">
        <UiButton @click="comparingConflict = !comparingConflict">
          Review the saved version
        </UiButton>
        <UiButton @click="useLatest">
          Use the saved version
        </UiButton>
        <UiButton variant="primary" :disabled="!ready || saving" @click="saveDraftOverLatest">
          Save my draft as the next version
        </UiButton>
      </div>
      <RuleFlow
        v-if="comparingConflict"
        :nodes="conflict.definition.steps"
        container="/steps"
        :catalog="catalog"
        :agent-name="agentName"
      />
    </section>
    <div v-if="definition" class="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_420px] gap-6 pt-5 max-[1100px]:grid-cols-1">
      <div class="min-h-0 overflow-y-auto pr-1 pb-16 [scrollbar-width:thin]">
        <div class="mx-auto grid max-w-190 gap-4">
          <textarea
            v-model="definition.description"
            rows="1"
            class="min-h-5! w-full resize-none border-0! bg-transparent! p-0! text-sm text-muted shadow-none! outline-none"
            placeholder="Add a description (optional)"
            aria-label="Description"
          />

          <!-- When -->
          <section class="trigger-card grid grid-cols-2 gap-4 rounded-[22px] border border-line bg-raised p-4 max-[700px]:grid-cols-1" aria-label="When">
            <div class="flex items-start gap-3">
              <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-ink text-canvas"><Icon :name="Zap" :size="19" /></span>
              <div class="min-w-0 flex-1">
                <p class="m-0! text-xs font-bold tracking-wide text-muted uppercase">
                  When
                </p>
                <select
                  class="mt-1 w-full"
                  :value="triggerKey(definition.trigger)"
                  aria-label="Hook"
                  @change="chooseTrigger(($event.target as HTMLSelectElement).value)"
                >
                  <option v-if="!hook" :value="triggerKey(definition.trigger)">
                    {{ definition.trigger.type === 'plugin' ? definition.trigger.hook : definition.trigger.hook }} (no longer available)
                  </option>
                  <optgroup v-for="[group, hooks] in hookGroups" :key="group" :label="group">
                    <option v-for="entry in hooks" :key="triggerKey(entry.trigger)" :value="triggerKey(entry.trigger)">
                      {{ entry.trigger.type === 'plugin' ? `${entry.instanceName} · ${entry.trigger.hook}` : entry.title }}
                    </option>
                  </optgroup>
                </select>
                <p v-if="hook" class="m-0! mt-1.5! text-xs text-muted">
                  {{ hook.description }}
                </p>
                <div v-if="payloadFields.length" class="mt-2.5 flex flex-wrap gap-1.5" aria-label="What the event carries">
                  <span
                    v-for="field in payloadFields.slice(0, 10)"
                    :key="field.expression"
                    class="rounded-md bg-hover px-2 py-0.5 font-mono text-2xs text-muted"
                    :title="field.description"
                  >{{ field.label }}</span>
                </div>
              </div>
              <Icon :name="Webhook" :size="18" class="mt-1 text-muted" />
            </div>
            <div class="flex items-start gap-3 border-l border-line pl-4 max-[700px]:border-t max-[700px]:border-l-0 max-[700px]:pt-4 max-[700px]:pl-0">
              <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-accent/12 text-accent"><Icon :name="Bot" :size="19" /></span>
              <div class="min-w-0 flex-1">
                <p class="m-0! text-xs font-bold tracking-wide text-muted uppercase">
                  With the rights of
                </p>
                <div class="mt-1 flex items-center gap-2">
                  <AgentAvatar :name="agentName(definition.executionAgentId)" :identity="definition.executionAgentId" :size="26" />
                  <select v-model="definition.executionAgentId" class="min-w-0 flex-1" aria-label="Execution agent">
                    <option v-for="agent in executionAgents" :key="agent.agentId" :value="agent.agentId">
                      {{ agent.name }}
                    </option>
                    <option v-if="!executionAgents.some(agent => agent.agentId === definition!.executionAgentId)" :value="definition.executionAgentId">
                      {{ agentName(definition.executionAgentId) }}
                    </option>
                  </select>
                </div>
                <p class="m-0! mt-1.5! text-xs text-muted">
                  Its grants decide which events reach the rule and which tools it calls. It does not work by itself.
                </p>
              </div>
            </div>
            <p v-for="problem in general.filter(entry => entry.path.startsWith('/trigger') || entry.path.startsWith('/executionAgentId'))" :key="problem.path + problem.message" class="col-span-full m-0! mt-3! flex items-start gap-1.5 text-xs font-semibold text-coral">
              <Icon :name="CircleAlert" :size="14" class="mt-px shrink-0" />{{ problem.message }}
            </p>
          </section>

          <div v-if="warnings.length" class="grid gap-2">
            <p
              v-for="warning in warnings"
              :key="warning.message"
              class="m-0! flex items-start gap-2.5 rounded-2xl border border-[#e7b84d]/50 bg-[#fff7e0] p-3.5 text-sm text-[#7a5600] dark:bg-[#3a2e10] dark:text-[#ffd57a]"
              role="note"
            >
              <Icon :name="ShieldAlert" :size="18" class="mt-px shrink-0" />{{ warning.message }}
            </p>
          </div>

          <!-- Then -->
          <section aria-label="Then">
            <p class="m-0! mb-1! pl-1 text-xs font-bold tracking-wide text-muted uppercase">
              Then
            </p>
            <RuleFlow
              :nodes="definition.steps"
              container="/steps"
              :catalog="catalog"
              :agent-name="agentName"
              editable
              :selected="selected"
              :problems="stepProblems"
              @select="path => selected = path"
              @insert="insert"
              @remove="remove"
              @move="move"
            />
            <p v-if="!definition.steps.length" class="m-0! mt-2! text-center text-sm text-muted">
              Add the first step with +.
            </p>
            <p v-for="problem in problemsUnder(problems, '/steps').filter(entry => entry.path === '/steps')" :key="problem.message" class="m-0! mt-2! text-center text-xs font-semibold text-coral">
              {{ problem.message }}
            </p>
          </section>

          <!-- Limits -->
          <section class="rounded-[22px] border border-line bg-raised p-4" aria-label="Limits">
            <p class="m-0! text-xs font-bold tracking-wide text-muted uppercase">
              Limits per hour
            </p>
            <div class="mt-3 grid grid-cols-2 gap-4 phone:grid-cols-1">
              <label class="grid gap-1.5">
                <span class="flex justify-between text-sm"><span>Triggerings that act</span><b>{{ definition.limits.triggeringsPerHour }}</b></span>
                <input
                  v-model.number="definition.limits.triggeringsPerHour"
                  type="range"
                  min="1"
                  :max="catalog?.limits.maxTriggeringsPerHour ?? 600"
                  class="accent-[var(--color-accent)]"
                >
              </label>
              <label class="grid gap-1.5">
                <span class="flex justify-between text-sm"><span>Conversations started or steered</span><b>{{ definition.limits.conversationsPerHour }}</b></span>
                <input
                  v-model.number="definition.limits.conversationsPerHour"
                  type="range"
                  min="1"
                  :max="catalog?.limits.maxConversationsPerHour ?? 100"
                  class="accent-[var(--color-accent)]"
                >
              </label>
            </div>
            <p class="m-0! mt-3! text-xs text-muted">
              Beyond either limit the rule pauses itself and tells you, so a loop stops on its own. Triggerings that end without acting are not counted.
            </p>
          </section>
          <p v-for="problem in general.filter(entry => !entry.path.startsWith('/trigger') && !entry.path.startsWith('/executionAgentId'))" :key="problem.path + problem.message" class="m-0! flex items-start gap-1.5 text-xs font-semibold text-coral">
            <Icon :name="CircleAlert" :size="14" class="mt-px shrink-0" />{{ problem.message }}
          </p>
        </div>
      </div>
      <aside v-if="wide" class="min-h-0 overflow-y-auto rounded-[22px] border border-line bg-raised p-5 [scrollbar-width:thin]" aria-label="Step">
        <RuleStepEditor
          v-if="selectedNode && selected"
          :key="selected"
          v-model="selectedNode"
          :path="selected"
          :catalog="catalog"
          :execution-agent-id="definition.executionAgentId"
          :variables="selectedVariables"
          :people-source="peopleSource"
          :problems="problems"
          @close="selected = null"
          @remove="remove(selected!)"
        />
        <div v-else class="grid gap-3 text-sm text-muted">
          <h2 class="m-0! font-heading text-lg font-bold text-ink">
            Build the flow
          </h2>
          <p class="m-0!">
            Add steps with <b class="text-ink">+</b>, then select a step to edit it here. Steps run from top to bottom; an <b class="text-ink">If</b> chooses one of its two paths.
          </p>
          <p class="m-0!">
            Every value can be text with variables such as <code class="rounded bg-hover px-1">{{ '{' }}{{ '{' }}payload.author{{ '}' }}{{ '}' }}</code>, a fixed value, or a formula. The <b class="text-ink">x</b> button lists what the event and earlier steps provide.
          </p>
          <p v-if="!problems.length && definition.steps.length" class="m-0! flex items-center gap-1.5 font-semibold text-success">
            <Icon :name="CircleCheck" :size="15" />The rule is ready to save.
          </p>
        </div>
      </aside>
    </div>
    <p v-else-if="!error" class="mt-6 text-sm text-muted" role="status">
      Loading the editor…
    </p>
    <Modal v-if="pendingLeave" title="Discard your unsaved changes?" @close="pendingLeave = null">
      <div class="px-6 py-5 text-sm text-muted">
        Your rule has changes that have not been saved yet.
      </div>
      <footer class="flex justify-end gap-2 border-t border-line px-6 py-4">
        <UiButton @click="pendingLeave = null">
          Keep editing
        </UiButton>
        <UiButton variant="danger" @click="confirmLeave">
          Discard changes
        </UiButton>
      </footer>
    </Modal>
    <Modal
      v-if="!wide && selectedNode && selected"
      title="Edit step"
      sheet
      headless
      @close="selected = null"
    >
      <div class="px-5 pt-5 pb-6">
        <RuleStepEditor
          :key="selected"
          v-model="selectedNode"
          :path="selected"
          :catalog="catalog"
          :execution-agent-id="definition!.executionAgentId"
          :variables="selectedVariables"
          :people-source="peopleSource"
          :problems="problems"
          @close="selected = null"
          @remove="remove(selected!)"
        />
      </div>
    </Modal>
  </div>
</template>
