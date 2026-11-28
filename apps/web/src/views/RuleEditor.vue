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
import PluginMark from '../components/PluginMark.vue'
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
  sameRuleDefinition,
  triggerKey,
  variablesAt,
  withInserted,
  withMoved,
  withRemoved,
  withReplaced,
} from '../hook-rules'
import {
  ArrowLeft,
  CircleAlert,
  CircleCheck,
  LoaderCircle,
  Search,
  Settings2,
  ShieldAlert,
  Zap,
} from '../icons'
import { ruleJsonDraftProblems } from '../rule-json-drafts'

// The visual rule editor: the rule is drawn on a canvas, from the node of what starts it
// (an event, with whose rights) through its steps, each edited where it is drawn. The
// installation checks the rule as it is written; nothing is shown as wrong before the
// person tries to save.
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
const creationDefinition = ref<HookRuleDefinition | null>(null)
const loadedDefinition = ref('')
const validated = ref(false)
const validationError = ref('')
let disposed = false
let leavingAfterSave = false
const pendingLeave = ref<string | null>(null)
const conflict = ref<HookRule | null>(null)
const comparingConflict = ref(false)
const removingId = ref<string | null>(null)
// A new rule starts from an empty node: nothing listens until the person picks an event.
const triggerChosen = ref(true)
const picker = ref<'event' | 'agent' | 'settings' | null>(null)
const eventQuery = ref('')
const attempted = ref(false)
const attention = ref<string | null>(null)
const nudge = ref('')
const celebrating = ref(false)
let nudgeTimer: ReturnType<typeof setTimeout> | undefined

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

    if (!loadedRule && !loadedCatalog.agents.length) {
      error.value = 'You need access to an agent before creating a rule. Ask the owner to give you access.'
      return
    }

    if (loadedRule) {
      definition.value = structuredClone(loadedRule.definition)
    }
    else {
      const requestedAgent = typeof route.query.agent === 'string' ? route.query.agent : ''
      const availableHooks = requestedAgent ? loadedCatalog.hooks.filter(entry => entry.agentIds.includes(requestedAgent)) : loadedCatalog.hooks
      // A plugin page asks for one of its instances: its first hook is the obvious start.
      const requested = availableHooks.find(entry => entry.trigger.type === 'plugin' && entry.trigger.instanceId === route.query.instance)
      const hook = requested ?? availableHooks.find(entry => entry.trigger.type === 'plugin') ?? availableHooks[0]
      const agent = requestedAgent && (!hook || hook.agentIds.includes(requestedAgent)) ? requestedAgent : hook?.agentIds[0] ?? loadedCatalog.agents[0]?.agentId ?? ''
      definition.value = emptyDefinition(hook?.trigger ?? { type: 'cairn', hook: 'replyReady' }, agent, loadedCatalog.limits.defaultLimits)
      triggerChosen.value = !!requested
    }

    loadedDefinition.value = JSON.stringify(definition.value)
  }
  catch (e) {
    error.value = (e as Error).message
  }
}

// Hooks the person may listen to, grouped by the plugin instance they come from.
const hookGroups = computed(() => {
  const query = eventQuery.value.trim().toLowerCase()
  const groups = new Map<string, {
    key: string
    name: string
    pluginId?: string | null
    brand?: { color: string, path?: string } | null
    hooks: NonNullable<typeof catalog.value>['hooks']
  }>()
  for (const entry of catalog.value?.hooks ?? []) {
    const name = entry.trigger.type === 'cairn' ? 'Cairn conversations' : entry.instanceName ?? 'Plugins'
    if (query && !`${entry.title} ${entry.description} ${name}`.toLowerCase().includes(query))
      continue
    const key = entry.trigger.type === 'cairn' ? 'cairn' : entry.trigger.instanceId
    const group = groups.get(key) ?? {
      key,
      name,
      pluginId: entry.pluginId,
      brand: entry.brand,
      hooks: [],
    }
    group.hooks.push(entry)
    groups.set(key, group)
  }

  return [...groups.values()]
})
const hook = computed(() => definition.value ? catalogHook(catalog.value, definition.value.trigger) : undefined)
const peopleSource = computed(() => {
  const field = hook.value?.resourceFields.find(entry => /person|user/i.test(entry.resourceType) || /author|sender/i.test(entry.path))
  return field && hook.value?.pluginId && definition.value?.trigger.type === 'plugin'
    ? {
        pluginId: hook.value.pluginId,
        instanceId: definition.value.trigger.instanceId,
        resourceType: field.resourceType,
        agentId: definition.value.executionAgentId,
      }
    : undefined
})

function chooseTrigger(key: string) {
  const next = catalog.value?.hooks.find(entry => triggerKey(entry.trigger) === key)
  if (!next || !definition.value)
    return
  const trigger: HookRuleTrigger = next.trigger
  const kept = next.agentIds.includes(definition.value.executionAgentId)
  const agent = kept ? definition.value.executionAgentId : next.agentIds[0] ?? definition.value.executionAgentId
  definition.value = { ...definition.value, trigger, executionAgentId: agent }
  triggerChosen.value = true
  eventQuery.value = ''
  // Ask whose rights only when the event did not settle it and there is a real choice.
  picker.value = !kept && next.agentIds.length > 1 ? 'agent' : null
}

function chooseAgent(agentId: string) {
  if (definition.value && hook.value?.agentIds.includes(agentId))
    definition.value = { ...definition.value, executionAgentId: agentId }
  picker.value = null
}

function say(message: string) {
  nudge.value = message
  clearTimeout(nudgeTimer)
  nudgeTimer = setTimeout(() => nudge.value = '', 3200)
}

// Shakes a node once; the same node can be shown again later.
function point(path: string | null) {
  attention.value = null
  if (path)
    requestAnimationFrame(() => attention.value = path)
}

// The node a problem belongs to: the deepest step its path goes through.
function problemNode(path: string): string | null {
  if (!definition.value || !path.startsWith('/steps/'))
    return null
  let found: string | null = null
  let current = ''
  for (const part of path.split('/').slice(1)) {
    current += `/${part}`
    if (/^\d+$/.test(part) && nodeAt(definition.value, current))
      found = current
  }

  return found
}

// Validation follows editing, a moment after the last change.
let timer: ReturnType<typeof setTimeout> | undefined
let generation = 0

// A check that answers during a press waits for the click that ends it: its status and messages
// change the layout, which would move the pressed control from under the pointer and lose the
// press. A press released elsewhere or cancelled lets the check through, and none is held longer
// than this after it answers or, without an answer yet, after the release.
const PRESS_HOLD = 2000
// Controls a press ends with a click on, which a tap delivers last. Dragging a slider has no click
// to protect.
const CLICKABLE = 'button, a[href], summary, label, input, select, textarea, [role="button"], [role="menuitem"], [role="option"], [role="tab"]'
const SLIDER = 'input[type="range"]'
let pressedPointerId: number | undefined
let pressedElement: Element | undefined
let held: (() => void) | undefined
let heldTimer: ReturnType<typeof setTimeout> | undefined

function afterPress(apply: () => void) {
  if (pressedPointerId === undefined) {
    apply()
    return
  }

  held = apply
  clearTimeout(heldTimer)
  heldTimer = setTimeout(releasePress, PRESS_HOLD)
}

function releasePress() {
  pressedPointerId = undefined
  pressedElement = undefined
  clearTimeout(heldTimer)
  const apply = held
  held = undefined
  apply?.()
}

function startPress(event: PointerEvent) {
  if (!event.isPrimary)
    return
  const target = event.target instanceof Element ? event.target : null
  const control = target?.closest(SLIDER) ? null : target?.closest(CLICKABLE)
  // A held check waits for this press's click; any other press ends an earlier one that had none.
  if (event.button !== 0 || !control || control.matches(':disabled')) {
    releasePress()
    return
  }

  // An earlier press that had no click no longer bounds this one.
  if (!held)
    clearTimeout(heldTimer)
  pressedPointerId = event.pointerId
  pressedElement = control
}

function finishPress(event: PointerEvent) {
  if (event.pointerId !== pressedPointerId)
    return
  const released = document.elementFromPoint(event.clientX, event.clientY)
  // Touch compatibility clicks may arrive after pointerup's next timer task, so a release on the
  // pressed control waits for its click. A release away from it has no click to protect.
  if (!released || !pressedElement?.contains(released)) {
    releasePress()
    return
  }

  if (!held) {
    clearTimeout(heldTimer)
    heldTimer = setTimeout(releasePress, PRESS_HOLD)
  }
}

// In the capture phase, a click is seen once its target is fixed and before its control handles it.
const pressEvents = [
  ['pointerdown', startPress],
  ['pointerup', finishPress],
  ['click', releasePress],
  ['pointercancel', releasePress],
  ['contextmenu', releasePress],
  ['dragstart', releasePress],
] as const

function scheduleValidation(value: HookRuleDefinition | null, delay = 350) {
  if (!value)
    return
  clearTimeout(timer)
  const current = ++generation
  validated.value = false
  validationError.value = ''
  checking.value = true
  timer = setTimeout(async () => {
    let apply: () => void
    try {
      const local = ruleJsonDraftProblems(value)
      const output = local.length
        ? { valid: false, problems: local, warnings: [] }
        : await validateHookRule(api, { definition: value, ruleId: rule.value?.id ?? id.value })
      apply = () => {
        validated.value = true
        validationError.value = ''
        problems.value = output.problems
        warnings.value = output.warnings
      }
    }
    catch (e) {
      apply = () => validationError.value = (e as Error).message
    }

    if (!disposed && current === generation) {
      afterPress(() => {
        if (disposed || current !== generation)
          return
        apply()
        checking.value = false
      })
    }
  }, delay)
}

watch(definition, (value) => {
  if (triggerChosen.value)
    scheduleValidation(value)
}, { deep: true })
watch(triggerChosen, (chosen) => {
  if (chosen)
    scheduleValidation(definition.value, 0)
})

// A rule being created is not judged before its first save attempt.
const showProblems = computed(() => !!rule.value || attempted.value)
const general = computed(() => showProblems.value ? problems.value.filter(problem => !problem.path.startsWith('/steps/')) : [])
const stepProblems = computed(() => showProblems.value ? problems.value.filter(problem => problem.path.startsWith('/steps')) : [])
const ready = computed(() => triggerChosen.value && (!rule.value || rule.value.allowed.edit) && validated.value && !checking.value && problems.value.length === 0 && ruleJsonDraftProblems(definition.value).length === 0 && !!definition.value?.name.trim())

// Saving too early explains what is missing and points at it, instead of refusing silently.
function attemptSave() {
  if (saving.value || conflict.value || !definition.value)
    return
  attempted.value = true
  if (!triggerChosen.value) {
    say('Start by choosing what starts the rule.')
    point('trigger')
    return
  }

  if (!definition.value.name.trim() && hook.value) {
    definition.value.name = hook.value.title.slice(0, 100)
    return say('Named after its event. Check it, then save.')
  }

  if (ready.value) {
    void save()
    return
  }

  if (checking.value || !validated.value) {
    say(validationError.value ? 'The check failed. Retry it, then save.' : 'Checking the rule…')
    return
  }

  const first = ruleJsonDraftProblems(definition.value)[0] ?? problems.value[0]
  if (!first) {
    say('This rule can’t be saved yet.')
    return
  }

  say(first.message)
  point(first.path.startsWith('/trigger') || first.path.startsWith('/executionAgentId') ? 'trigger' : problemNode(first.path))
}

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
  const node = definition.value ? nodeAt(definition.value, path) : undefined
  if (node && ((node.type === 'if' && (node.then.length || node.otherwise.length)) || (node.type === 'forEach' && node.body.length))) {
    removingId.value = node.id
    return
  }

  mutateTree(definition => withRemoved(definition, path))
}

function confirmRemove() {
  const path = definition.value && removingId.value ? pathOfNode(definition.value, removingId.value) : null
  removingId.value = null
  if (path)
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
  if (!definition.value || saving.value || !ready.value)
    return
  const submitted = JSON.parse(JSON.stringify(definition.value)) as HookRuleDefinition
  saving.value = true
  error.value = ''
  try {
    let saved: HookRule
    if (rule.value) {
      try {
        saved = await replaceHookRule(api, { ruleId: rule.value.id, revision: rule.value.revision, definition: submitted })
      }
      catch (cause) {
        if (!(cause instanceof ApiError) || cause.status !== 409)
          throw cause
        const latest = await getHookRule(api, { ruleId: rule.value.id })
        if (sameRuleDefinition(latest.definition, submitted)) {
          saved = latest
        }
        else {
          if (!latest.allowed.edit) {
            rule.value = latest
            throw new Error('You can no longer edit this rule. Your draft is still here.')
          }

          if (latest.version !== rule.value.version) {
            conflict.value = latest
            return
          }

          rule.value = latest
          saved = await replaceHookRule(api, { ruleId: latest.id, revision: latest.revision, definition: submitted })
        }
      }
    }
    else {
      creationDefinition.value ??= submitted
      try {
        saved = await createHookRule(api, { creationId, definition: creationDefinition.value, enabled: true })
      }
      catch (cause) {
        if (cause instanceof ApiError && cause.status >= 400 && cause.status < 500 && cause.status !== 408)
          creationDefinition.value = null
        throw cause
      }
    }

    if (typeof saved.id !== 'string' || !saved.id || !saved.definition || typeof saved.allowed?.edit !== 'boolean')
      throw new Error('The save response was incomplete. Your draft is still here; retry saving.')
    if (!sameRuleDefinition(saved.definition, definition.value)) {
      rule.value = saved
      conflict.value = saved.allowed.edit ? saved : null
      error.value = 'Your save succeeded. Your later edits are preserved; save them as the next version.'
      return
    }

    notify(rule.value ? 'Rule saved as a new version' : 'Rule created')
    leavingAfterSave = true
    if (!rule.value) {
      // A brief moment for a new rule: three stones settle, then its page opens.
      celebrating.value = true
      const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches
      await new Promise(resolve => setTimeout(resolve, still ? 300 : 1100))
    }

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
  for (const [type, listener] of pressEvents)
    window.addEventListener(type, listener as EventListener, true)
  window.addEventListener('blur', releasePress)
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
  clearTimeout(heldTimer)
  clearTimeout(nudgeTimer)
  for (const [type, listener] of pressEvents)
    window.removeEventListener(type, listener as EventListener, true)
  window.removeEventListener('blur', releasePress)
  wideQuery.removeEventListener('change', onWide)
  window.removeEventListener('beforeunload', beforeUnload)
})
</script>

<template>
  <div class="rule-editor relative flex min-h-0 flex-1 flex-col">
    <header class="flex flex-wrap items-center gap-3 border-b border-line pb-3">
      <!-- On narrow screens, validation must not change the title's wrapping mid-press. -->
      <div class="flex min-w-0 flex-1 items-center gap-3 max-[480px]:basis-full">
        <RouterLink :to="rule ? { path: '/rules', query: { rule: rule.id } } : '/rules'" class="grid size-10 shrink-0 place-items-center rounded-full border border-line text-ink hover:bg-hover" aria-label="Back to the rules">
          <Icon :name="ArrowLeft" :size="18" />
        </RouterLink>
        <div class="min-w-0 flex-1">
          <p class="m-0! text-xs font-semibold tracking-wide text-muted uppercase">
            {{ rule ? `Editing version ${rule.version}` : 'New rule' }}
          </p>
          <input
            v-if="definition"
            v-model="definition.name"
            class="name-input w-full! border-0! bg-transparent! p-0! font-heading text-[24px] font-bold tracking-[-0.5px] shadow-none! outline-none"
            maxlength="100"
            :placeholder="hook && triggerChosen ? hook.title : 'Name your rule'"
            aria-label="Rule name"
          >
        </div>
      </div>
      <span
        v-if="triggerChosen"
        class="inline-flex items-center gap-1.5 rounded-full px-3 py-1.5 text-xs font-semibold"
        :class="checking || !validated && !validationError || !showProblems && problems.length ? 'bg-hover text-muted' : validationError || problems.length ? 'bg-coral/12 text-coral' : 'bg-success/12 text-success'"
        role="status"
      >
        <Icon :name="checking ? LoaderCircle : validationError || problems.length && showProblems ? CircleAlert : CircleCheck" :size="14" :class="checking ? 'animate-spin' : ''" />
        {{ checking ? 'Checking…' : validationError ? 'Check failed' : !validated ? 'Waiting for check' : !problems.length ? 'Ready' : showProblems ? `${problems.length} to fix` : 'Draft' }}
      </span>
      <button
        type="button"
        class="grid size-10 shrink-0 place-items-center rounded-full border border-line text-muted hover:bg-hover hover:text-ink max-[480px]:ml-auto"
        aria-label="Limits and description"
        :disabled="!definition"
        @click="picker = 'settings'"
      >
        <Icon :name="Settings2" :size="17" />
      </button>
      <UiButton
        variant="primary"
        :disabled="saving || !definition"
        :aria-disabled="!ready || !!conflict"
        :class="!ready || conflict ? 'opacity-45' : ''"
        @click="attemptSave"
      >
        {{ saving ? 'Saving…' : rule ? 'Save version' : 'Create rule' }}
      </UiButton>
    </header>
    <UiAlert v-if="error" class="mt-3">
      {{ error }}
    </UiAlert>
    <UiAlert v-if="validationError" class="mt-3">
      {{ validationError }}
      <UiButton size="small" @click="scheduleValidation(definition, 0)">
        Retry validation
      </UiButton>
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
    <div v-if="warnings.length && triggerChosen" class="mt-3 grid shrink-0 gap-2">
      <p
        v-for="warning in warnings"
        :key="warning.message"
        class="m-0! flex items-start gap-2.5 rounded-2xl border border-[#e7b84d]/50 bg-[#fff7e0] p-3 text-sm text-[#7a5600] dark:bg-[#3a2e10] dark:text-[#ffd57a]"
        role="note"
      >
        <Icon :name="ShieldAlert" :size="18" class="mt-px shrink-0" />{{ warning.message }}
      </p>
    </div>
    <div v-if="definition" class="grid min-h-0 flex-1 gap-6 pt-4" :class="wide && selectedNode ? 'grid-cols-[minmax(0,1fr)_360px]' : 'grid-cols-1'">
      <section class="min-h-[420px]" aria-label="Rule graph">
        <RuleFlow
          fill
          :nodes="definition.steps"
          container="/steps"
          :catalog="catalog"
          :agent-name="agentName"
          editable
          :selected="selected"
          :execution-agent-id="definition.executionAgentId"
          :problems="stepProblems"
          :attention="attention"
          @select="path => selected = path"
          @insert="insert"
          @remove="remove"
          @move="move"
        >
          <template #start>
            <div
              class="start-node relative w-64 rounded-[20px] border bg-raised text-left shadow-[0_2px_8px_rgb(0_0_0/0.05)]"
              :class="[triggerChosen ? (general.some(entry => entry.path.startsWith('/trigger') || entry.path.startsWith('/executionAgentId')) ? 'border-coral/70' : 'border-line') : 'empty', attention === 'trigger' ? 'attention' : '']"
            >
              <button
                type="button"
                class="flex w-full items-start gap-3 rounded-[20px] p-3.5 text-left focus-visible:outline-2 focus-visible:outline-accent"
                :aria-label="triggerChosen && hook ? `Change what starts the rule: ${hook.title}` : 'Choose what starts the rule'"
                @click="picker = 'event'"
              >
                <PluginMark
                  v-if="triggerChosen && hook?.pluginId"
                  :plugin="hook.pluginId"
                  :brand="hook.brand"
                  :size="36"
                />
                <span v-else class="grid size-9 shrink-0 place-items-center rounded-xl bg-accent/12 text-accent"><Icon :name="Zap" :size="18" /></span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-bold tracking-wide text-accent uppercase">When</span>
                  <span class="block font-heading text-[15px] leading-tight font-semibold">{{ triggerChosen ? hook?.title ?? (definition.trigger.type === 'plugin' ? `${definition.trigger.hook} (no longer available)` : definition.trigger.hook) : 'Choose what starts the rule' }}</span>
                  <span class="mt-0.5 block truncate text-xs text-muted">{{ triggerChosen ? hook?.instanceName ?? 'Cairn conversations' : 'A message, a pull request, a mail…' }}</span>
                </span>
              </button>
              <button
                v-if="triggerChosen"
                type="button"
                class="mx-3.5 mb-3 -mt-1 inline-flex max-w-[calc(100%-28px)] items-center gap-1.5 rounded-full bg-hover py-0.5 pr-2.5 pl-0.5 text-xs font-semibold hover:bg-soft"
                aria-label="Execution agent"
                :title="`With the rights of ${agentName(definition.executionAgentId)}`"
                @click="picker = 'agent'"
              >
                <AgentAvatar :name="agentName(definition.executionAgentId)" :identity="definition.executionAgentId" :size="20" />
                <span class="truncate">{{ agentName(definition.executionAgentId) }}</span>
              </button>
              <p v-for="problem in general.filter(entry => entry.path.startsWith('/trigger') || entry.path.startsWith('/executionAgentId'))" :key="problem.path + problem.message" class="m-0! flex items-start gap-1.5 px-3.5 pb-3 text-xs font-semibold text-coral">
                <Icon :name="CircleAlert" :size="14" class="mt-px shrink-0" />{{ problem.message }}
              </p>
            </div>
          </template>
        </RuleFlow>
        <p v-for="problem in stepProblems.filter(entry => entry.path === '/steps')" :key="problem.message" class="m-0! mt-2! text-center text-xs font-semibold text-coral">
          {{ problem.message }}
        </p>
        <p v-for="problem in general.filter(entry => !entry.path.startsWith('/trigger') && !entry.path.startsWith('/executionAgentId'))" :key="problem.path + problem.message" class="m-0! mt-2! flex items-start gap-1.5 text-xs font-semibold text-coral">
          <Icon :name="CircleAlert" :size="14" class="mt-px shrink-0" />{{ problem.message }}
        </p>
      </section>
      <aside v-if="wide && selectedNode" class="min-h-0 overflow-y-auto rounded-[22px] border border-line bg-raised p-5 [scrollbar-width:thin]" aria-label="Step">
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
      </aside>
    </div>
    <p v-else-if="!error" class="mt-6 text-sm text-muted" role="status">
      Loading the editor…
    </p>
    <Modal v-if="removingId" title="Remove this step and its branches?" @close="removingId = null">
      <p class="px-6 py-5 text-sm text-muted">
        All steps inside this block will also be removed.
      </p>
      <footer class="flex justify-end gap-2 border-t border-line px-6 py-4">
        <UiButton @click="removingId = null">
          Keep the step
        </UiButton>
        <UiButton variant="danger" @click="confirmRemove">
          Remove the block
        </UiButton>
      </footer>
    </Modal>
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
      v-if="picker === 'event'"
      title="What starts the rule?"
      sheet
      @close="picker = null"
    >
      <div class="grid gap-1 px-4 pt-3 pb-5">
        <label class="mx-1 mb-1 flex min-h-11 items-center gap-2 rounded-xl bg-hover px-3 text-muted">
          <Icon :name="Search" :size="16" />
          <input
            v-model="eventQuery"
            class="w-full! min-w-0 flex-1 border-0! bg-transparent! p-0! text-ink shadow-none! outline-none"
            placeholder="Search: message, pull request, mail…"
            aria-label="Search events"
          >
        </label>
        <section v-for="group in hookGroups" :key="group.key" :aria-label="group.name">
          <h3 class="m-0! flex items-center gap-2 px-2 pt-3 pb-1 text-xs font-bold text-muted">
            <PluginMark
              v-if="group.pluginId"
              :plugin="group.pluginId"
              :brand="group.brand"
              :size="18"
            />
            <Icon
              v-else
              :name="Zap"
              :size="15"
              class="text-accent"
            />
            {{ group.name }}
          </h3>
          <button
            v-for="entry in group.hooks"
            :key="triggerKey(entry.trigger)"
            type="button"
            class="flex w-full items-center gap-3 rounded-xl px-2 py-2.5 text-left hover:bg-hover"
            :aria-pressed="triggerChosen && triggerKey(entry.trigger) === triggerKey(definition!.trigger)"
            @click="chooseTrigger(triggerKey(entry.trigger))"
          >
            <span class="min-w-0 flex-1">
              <span class="block text-sm font-semibold">{{ entry.title }}</span>
              <span class="block text-xs text-muted">{{ entry.description }}</span>
            </span>
            <Icon
              v-if="triggerChosen && triggerKey(entry.trigger) === triggerKey(definition!.trigger)"
              :name="CircleCheck"
              :size="17"
              class="shrink-0 text-success"
            />
          </button>
        </section>
        <p v-if="!hookGroups.length" class="m-0! px-2 py-6 text-center text-sm text-muted">
          No event matches. Agents only offer the hooks the owner granted them.
        </p>
      </div>
    </Modal>
    <Modal
      v-if="picker === 'agent' && definition"
      title="With whose rights?"
      sheet
      @close="picker = null"
    >
      <div class="grid gap-1 px-4 pt-3 pb-5">
        <button
          v-for="agent in catalog?.agents ?? []"
          :key="agent.agentId"
          type="button"
          class="flex w-full items-center gap-3 rounded-xl px-2 py-2.5 text-left hover:bg-hover disabled:cursor-default disabled:opacity-45 disabled:hover:bg-transparent"
          :disabled="!!hook && !hook.agentIds.includes(agent.agentId)"
          :aria-pressed="definition.executionAgentId === agent.agentId"
          @click="chooseAgent(agent.agentId)"
        >
          <AgentAvatar :name="agent.name" :identity="agent.agentId" :size="34" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm font-semibold">{{ agent.name }}</span>
            <span v-if="hook && !hook.agentIds.includes(agent.agentId)" class="block text-xs text-muted">Can’t listen to this event</span>
          </span>
          <Icon
            v-if="definition.executionAgentId === agent.agentId"
            :name="CircleCheck"
            :size="17"
            class="shrink-0 text-success"
          />
        </button>
        <p class="m-0! px-2 pt-2 text-xs text-muted">
          The rule acts with this agent’s rights: the tools it calls are the agent’s.
        </p>
      </div>
    </Modal>
    <Modal
      v-if="picker === 'settings' && definition"
      title="Limits and description"
      sheet
      @close="picker = null"
    >
      <div class="grid gap-4 px-6 py-5">
        <label class="grid gap-1.5 text-sm">
          <span class="font-semibold">Description</span>
          <textarea
            v-model="definition.description"
            rows="3"
            placeholder="What this rule is for (optional)"
            aria-label="Description"
          />
        </label>
        <div class="grid grid-cols-2 gap-4 phone:grid-cols-1" aria-label="Limits">
          <label class="grid gap-1.5">
            <span class="flex justify-between text-sm"><span>Triggerings that act per hour</span><b>{{ definition.limits.triggeringsPerHour }}</b></span>
            <input
              v-model.number="definition.limits.triggeringsPerHour"
              type="range"
              min="1"
              :max="catalog?.limits.maxTriggeringsPerHour ?? 600"
              class="accent-[var(--color-accent)]"
            >
          </label>
          <label class="grid gap-1.5">
            <span class="flex justify-between text-sm"><span>Conversations started or steered per hour</span><b>{{ definition.limits.conversationsPerHour }}</b></span>
            <input
              v-model.number="definition.limits.conversationsPerHour"
              type="range"
              min="1"
              :max="catalog?.limits.maxConversationsPerHour ?? 100"
              class="accent-[var(--color-accent)]"
            >
          </label>
        </div>
        <p class="m-0! text-xs text-muted">
          Beyond either limit the rule pauses itself and tells you, so a loop stops on its own. Triggerings that end without acting are not counted.
        </p>
      </div>
    </Modal>
    <p
      v-if="nudge"
      class="nudge pointer-events-none absolute bottom-6 left-1/2 z-20 m-0! -translate-x-1/2 rounded-xl bg-ink px-4 py-2.5 text-sm font-semibold text-canvas shadow-lg"
      role="status"
      aria-live="polite"
    >
      {{ nudge }}
    </p>
    <div v-if="celebrating" class="celebration absolute inset-0 z-30 grid place-items-center bg-canvas/92 text-center" role="status">
      <div>
        <div class="stones mx-auto mb-5 grid justify-items-center gap-[3px]" aria-hidden="true">
          <i class="h-[11px] w-[22px] bg-success" /><i class="h-[13px] w-8 bg-ink" /><i class="h-[15px] w-11 bg-ink" />
        </div>
        <p class="m-0! font-heading text-xl font-bold">
          Your rule is active
        </p>
      </div>
    </div>
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

<style scoped>
.start-node.empty { border: 2px dashed var(--color-accent); background: color-mix(in srgb, var(--color-accent) 6%, var(--color-raised)); animation: start-breathe 2.4s ease-in-out infinite; }
@keyframes start-breathe { 50% { box-shadow: 0 0 0 7px color-mix(in srgb, var(--color-accent) 12%, transparent); } }
.attention { animation: start-attention .5s ease; }
@keyframes start-attention { 20%, 60% { transform: translateX(-6px) } 40%, 80% { transform: translateX(6px) } }
.nudge { animation: nudge-in .2s ease; }
@keyframes nudge-in { from { opacity: 0; transform: translate(-50%, 8px); } }
.stones i { display: block; border-radius: 50%; opacity: 0; transform: translateY(-14px); animation: stone-settle .42s cubic-bezier(.3, 1.3, .5, 1) forwards; }
.stones i:nth-child(1) { animation-delay: .5s; } .stones i:nth-child(2) { animation-delay: .3s; } .stones i:nth-child(3) { animation-delay: .1s; }
@keyframes stone-settle { to { opacity: 1; transform: none; } }
@media (prefers-reduced-motion: reduce) {
  .start-node.empty, .attention, .nudge { animation: none; }
  .attention { outline: 2px solid var(--color-accent); }
  .stones i { animation: none; opacity: 1; transform: none; }
}
</style>
