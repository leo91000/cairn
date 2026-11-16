<script setup lang="ts">
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleProblem } from '../../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { RuleNode } from '../../../../packages/contracts/generated/typescript/RuleNode'
import type { RuleValue } from '../../../../packages/contracts/generated/typescript/RuleValue'
import type { VariableOption } from '../hook-rules'
import { computed, ref } from 'vue'
import { state } from '../api'
import {
  catalogTool,
  describeAction,
  emptyValue,
  initialToolInput,
  optionalRuleText,
  ownProblems,
  stepChoice,
} from '../hook-rules'
import { Plus, Trash2, X } from '../icons'
import { resolveRuleSchema, ruleLiteralDefault, ruleToolInputSchema } from '../rule-schema'
import Icon from './Icon.vue'
import RuleConditionEditor from './RuleConditionEditor.vue'
import RuleValueInput from './RuleValueInput.vue'

// The form of one step, beside the flow on wide screens and in a sheet on phones.
const props = defineProps<{
  path: string
  catalog: HookRuleCatalog | null
  executionAgentId: string
  variables: VariableOption[]
  problems: HookRuleProblem[]
  peopleSource?: {
    pluginId: string
    instanceId: string
    resourceType: string
    agentId: string
  }
}>()
const emit = defineEmits<{ close: [], remove: [] }>()
const node = defineModel<RuleNode>({ required: true })

const action = computed(() => node.value.type === 'action' ? node.value.action : null)
const mine = computed(() => ownProblems(props.problems, props.path))

function problemsAt(suffix: string) {
  const path = `${props.path}${suffix}`
  return mine.value.filter(problem => problem.path === path || problem.path.startsWith(`${path}/`)).map(problem => problem.message)
}

function setAction(next: NonNullable<typeof action.value>) {
  if (node.value.type === 'action')
    node.value = { ...node.value, action: next }
}

// Templates lose type narrowing inside handlers: change fields of the current action.
function patch(changes: Record<string, unknown>) {
  if (action.value)
    setAction({ ...action.value, ...changes } as NonNullable<typeof action.value>)
}

// Tools the execution agent holds, grouped by instance.
const tools = computed(() => (props.catalog?.tools ?? []).filter(tool => tool.agentIds.includes(props.executionAgentId)))
const toolKey = computed(() => action.value?.type === 'callTool' ? `${action.value.instanceId}:${action.value.tool}` : '')
const tool = computed(() => action.value?.type === 'callTool' ? catalogTool(props.catalog, action.value.instanceId, action.value.tool) : undefined)

function chooseTool(key: string) {
  const chosen = tools.value.find(entry => `${entry.instanceId}:${entry.tool}` === key)
  if (action.value?.type !== 'callTool' || !chosen)
    return
  const input = initialToolInput(chosen.inputSchema)
  setAction({
    ...action.value,
    instanceId: chosen.instanceId,
    tool: chosen.tool,
    input,
  })
}

function resourceSource(name: string) {
  const path = `/${name.replace(/~/g, '~0').replace(/\//g, '~1')}`
  const field = tool.value?.resourceFields.find(entry => entry.path === path)
  return field && tool.value
    ? {
        pluginId: tool.value.pluginId,
        instanceId: tool.value.instanceId,
        resourceType: field.resourceType,
        agentId: props.executionAgentId,
      }
    : undefined
}

interface Field {
  name: string
  title: string
  description: string
  required: boolean
  expects: 'text' | 'number' | 'boolean' | 'json'
  schema: any
}

const fields = computed<Field[]>(() => {
  const root = tool.value?.inputSchema ?? {}
  const schema = ruleToolInputSchema(root)
  const required = new Set<string>(Array.isArray(schema.required) ? schema.required : [])
  return Object.entries<any>(schema.properties ?? {}).map(([name, raw]) => {
    const property = resolveRuleSchema(raw, root)
    const type = Array.isArray(property?.type) ? property.type.find((entry: string) => entry !== 'null') : property?.type
    return {
      name,
      title: property?.title ?? name,
      description: property?.description ?? '',
      required: required.has(name),
      expects: type === 'number' || type === 'integer' ? 'number' : type === 'boolean' ? 'boolean' : type === 'object' || type === 'array' ? 'json' : 'text',
      schema: property,
    }
  })
})
const shownFields = computed(() => fields.value.filter(field => field.required || (action.value?.type === 'callTool' && Object.hasOwn(action.value.input, field.name))))
const optionalFields = computed(() => fields.value.filter(field => !shownFields.value.includes(field)))

function defaultValue(field: Field): RuleValue {
  if ('default' in field.schema || field.schema.enum?.length)
    return { type: 'literal', value: ruleLiteralDefault(field.schema, tool.value?.inputSchema ?? field.schema) }
  if (field.expects === 'boolean')
    return { type: 'literal', value: true }
  if (field.expects === 'number')
    return { type: 'literal', value: 0 }
  if (field.expects === 'json')
    return { type: 'literal', value: ruleLiteralDefault(field.schema, tool.value?.inputSchema ?? field.schema) }
  return emptyValue()
}

function setInput(name: string, value: RuleValue | null) {
  if (action.value?.type !== 'callTool')
    return
  const input = value ? { ...action.value.input, [name]: value } : { ...action.value.input }
  if (!value)
    delete input[name]
  setAction({ ...action.value, input })
}

const agents = computed(() => props.catalog?.agents ?? [])

// A wait is edited as an amount and a unit when it is a fixed number of seconds.
const units = [{ value: 1, label: 'seconds' }, { value: 60, label: 'minutes' }, { value: 3600, label: 'hours' }, { value: 86400, label: 'days' }]
const selectedWaitUnit = ref<number | null>(null)
const wait = computed(() => {
  if (action.value?.type !== 'wait' || action.value.seconds.type !== 'literal' || typeof action.value.seconds.value !== 'number')
    return null
  const seconds = action.value.seconds.value
  const unit = units.find(entry => entry.value === selectedWaitUnit.value) ?? [...units].reverse().find(entry => seconds > 0 && seconds % entry.value === 0) ?? units[0]
  return { amount: seconds / unit.value, unit: unit.value }
})

function setWait(amount: number, unit: number) {
  if (action.value?.type === 'wait') {
    selectedWaitUnit.value = unit
    setAction({ type: 'wait', seconds: { type: 'literal', value: Math.round(amount * unit) } })
  }
}

function useWaitFormula() {
  if (wait.value)
    setAction({ type: 'wait', seconds: { type: 'formula', expression: String(wait.value.amount * wait.value.unit) } })
}

const title = computed(() => node.value.type === 'action' ? stepChoice(node.value.action.type).title : node.value.type === 'if' ? 'If / otherwise' : 'For each')
const agentName = (id: string) => props.catalog?.agents.find(agent => agent.agentId === id)?.name ?? 'an agent'
</script>

<template>
  <section class="step-editor grid content-start gap-4" :aria-label="`Edit ${title}`">
    <header class="flex items-start gap-3">
      <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-accent/12 text-accent">
        <Icon :name="stepChoice(node.type === 'action' ? node.action.type : node.type).icon" :size="19" />
      </span>
      <div class="min-w-0 flex-1">
        <h2 class="m-0! font-heading text-lg font-bold">
          {{ title }}
        </h2>
        <p class="m-0! text-xs text-muted">
          {{ stepChoice(node.type === 'action' ? node.action.type : node.type).description }}
        </p>
      </div>
      <button
        type="button"
        class="grid size-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-ink"
        aria-label="Close the step"
        @click="emit('close')"
      >
        <Icon :name="X" :size="16" />
      </button>
    </header>

    <div class="grid grid-cols-2 gap-3 phone:grid-cols-1">
      <label v-if="node.type === 'action'" class="grid gap-1.5">
        <span class="text-xs font-semibold">Label</span>
        <input
          :value="node.label ?? ''"
          maxlength="100"
          :placeholder="describeAction(node.action, catalog, agentName).title"
          @input="node = { ...node, label: ($event.target as HTMLInputElement).value }"
        >
        <small v-for="problem in problemsAt('/label')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      </label>
      <details class="grid gap-1.5">
        <summary class="cursor-pointer text-xs font-semibold text-muted">
          Advanced reference
        </summary><label class="grid gap-1.5 pt-2">
          <span class="text-xs font-semibold">Step name</span>
          <input
            :value="node.id"
            class="font-mono text-[13px]"
            maxlength="40"
            @input="node = { ...node, id: ($event.target as HTMLInputElement).value }"
          >
          <small class="text-2xs text-muted">Later steps read its result as <code>steps.{{ node.id }}</code>.</small>
          <small v-for="problem in problemsAt('/id')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
        </label>
      </details>
    </div>

    <!-- If / otherwise -->
    <RuleConditionEditor
      v-if="node.type === 'if'"
      :model-value="node.condition"
      :path="`${path}/condition`"
      :variables="variables"
      :problems="problems"
      :people-source="peopleSource"
      @update:model-value="condition => node.type === 'if' && (node = { ...node, condition })"
    />

    <!-- For each -->
    <template v-else-if="node.type === 'forEach'">
      <RuleValueInput
        :model-value="node.items"
        label="List to go through"
        expects="field"
        :variables="variables"
        :problems="problemsAt('/items')"
        hint="A list from the event or an earlier step, such as payload.attachments."
        @update:model-value="items => node.type === 'forEach' && (node = { ...node, items })"
      />
      <label class="grid gap-1.5">
        <span class="text-xs font-semibold">Name of each item</span>
        <input :value="node.itemName" class="font-mono text-[13px]" @input="node.type === 'forEach' && (node = { ...node, itemName: ($event.target as HTMLInputElement).value })">
        <small v-for="problem in problemsAt('/itemName')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      </label>
    </template>

    <!-- Actions -->
    <template v-else-if="action?.type === 'callTool'">
      <label class="grid gap-1.5">
        <span class="text-xs font-semibold">Tool</span>
        <select :value="toolKey" @change="chooseTool(($event.target as HTMLSelectElement).value)">
          <option v-if="!tool" :value="toolKey" disabled>{{ action.tool || 'Choose a tool' }}</option>
          <optgroup v-for="instance in [...new Set(tools.map(entry => entry.instanceName))]" :key="instance" :label="instance">
            <option v-for="entry in tools.filter(candidate => candidate.instanceName === instance)" :key="`${entry.instanceId}:${entry.tool}`" :value="`${entry.instanceId}:${entry.tool}`">{{ entry.tool }}</option>
          </optgroup>
        </select>
        <small v-if="tool" class="text-xs text-muted">{{ tool.description }}</small>
        <small v-else-if="!tools.length" class="text-xs text-muted">The execution agent holds no plugin tool. The owner grants tools on the agent’s page.</small>
        <small v-for="problem in problemsAt('/action/tool').concat(problemsAt('/action/instanceId'))" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      </label>
      <div v-if="tool" class="grid gap-3 rounded-2xl border border-line bg-surface p-3.5">
        <div v-for="field in shownFields" :key="field.name" class="grid gap-1">
          <RuleValueInput
            :model-value="action.input[field.name] ?? defaultValue(field)"
            :label="field.title"
            :expects="field.expects"
            :schema="field.schema"
            :schema-root="tool?.inputSchema"
            :resource-source="resourceSource(field.name)"
            :required="field.required"
            :hint="field.description"
            :multiline="field.expects === 'text' && /text|content|message|body|prompt/i.test(field.name)"
            :variables="variables"
            :problems="problemsAt(`/action/input/${field.name}`)"
            @update:model-value="value => setInput(field.name, value)"
          />
          <button
            v-if="!field.required"
            type="button"
            class="justify-self-end text-xs text-muted hover:text-coral"
            :aria-label="`Remove ${field.title}`"
            @click="setInput(field.name, null)"
          >
            Remove this field
          </button>
        </div>
        <div v-if="optionalFields.length" class="flex flex-wrap gap-1.5">
          <button
            v-for="field in optionalFields"
            :key="field.name"
            type="button"
            class="inline-flex items-center gap-1 rounded-full border border-dashed border-line px-2.5 py-1 text-xs text-muted hover:border-accent hover:text-accent"
            :title="field.description"
            @click="setInput(field.name, defaultValue(field))"
          >
            <Icon :name="Plus" :size="12" />{{ field.title }}
          </button>
        </div>
        <button
          v-for="name in Object.keys(action.input).filter(name => !fields.some(field => field.name === name))"
          :key="name"
          type="button"
          class="justify-self-start text-xs text-coral"
          @click="setInput(name, null)"
        >
          Remove unknown input {{ name }}
        </button>
        <small v-for="problem in mine.filter(entry => entry.path === `${path}/action/input`)" :key="problem.message" class="text-xs font-semibold text-coral">{{ problem.message }}</small>
      </div>
    </template>

    <template v-else-if="action?.type === 'startAgent'">
      <label class="grid gap-1.5">
        <span class="text-xs font-semibold">Agent</span>
        <select :value="action.agentId" @change="patch({ agentId: ($event.target as HTMLSelectElement).value })">
          <option v-for="agent in agents" :key="agent.agentId" :value="agent.agentId">{{ agent.name }}{{ agent.native ? ' · may run natively' : '' }}</option>
        </select>
        <small v-for="problem in problemsAt('/action/agentId')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      </label>
      <RuleValueInput
        :model-value="action.prompt"
        label="Message to the agent"
        multiline
        required
        :variables="variables"
        :problems="problemsAt('/action/prompt')"
        placeholder="Answer {{author}} in #support: {{payload.content}}"
        @update:model-value="prompt => patch({ prompt })"
      />
      <RuleValueInput
        :model-value="action.author ?? { type: 'formula', expression: '' }"
        label="External author (optional)"
        expects="field"
        :variables="variables"
        :problems="problemsAt('/action/author')"
        hint="Choose the event field holding the person who started this conversation."
        @update:model-value="author => patch({ author: author.type === 'formula' && !author.expression ? null : optionalRuleText(author) })"
      />
      <RuleValueInput
        :model-value="action.key ?? emptyValue()"
        label="Conversation key (optional)"
        :variables="variables"
        :problems="problemsAt('/action/key')"
        placeholder="discord:{{threadId}}"
        hint="Binds the conversation to a key, so later events can steer it or answer its questions."
        @update:model-value="key => patch({ key: optionalRuleText(key) })"
      />
      <label class="grid gap-1.5">
        <span class="text-xs font-semibold">Project</span>
        <select :value="action.projectId ?? ''" @change="patch({ projectId: ($event.target as HTMLSelectElement).value || null })">
          <option value="">
            No project
          </option>
          <option v-for="project in state.projects" :key="project.id" :value="project.id">{{ project.name }}</option>
        </select>
        <small v-for="problem in problemsAt('/action/projectId')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      </label>
    </template>

    <template v-else-if="action?.type === 'steerConversation' || action?.type === 'answerQuestion' || action?.type === 'cancelConversation'">
      <RuleValueInput
        :model-value="action.key"
        label="Conversation key"
        required
        :variables="variables"
        :problems="problemsAt('/action/key')"
        placeholder="discord:{{threadId}}"
        @update:model-value="key => patch({ key })"
      />
      <RuleValueInput
        v-if="action.type === 'steerConversation'"
        :model-value="action.message"
        label="Message"
        multiline
        required
        :variables="variables"
        :problems="problemsAt('/action/message')"
        @update:model-value="message => patch({ message })"
      />
      <RuleValueInput
        v-if="action.type === 'answerQuestion'"
        :model-value="action.answer"
        label="Answer"
        multiline
        required
        :variables="variables"
        :problems="problemsAt('/action/answer')"
        @update:model-value="answer => patch({ answer })"
      />
    </template>

    <template v-else-if="action?.type === 'storeValue'">
      <div class="grid grid-cols-2 gap-3 phone:grid-cols-1">
        <label class="grid gap-1.5">
          <span class="text-xs font-semibold">Name</span>
          <input
            :value="action.name"
            aria-label="Name"
            class="font-mono text-[13px]"
            maxlength="40"
            @input="patch({ name: ($event.target as HTMLInputElement).value })"
          >
          <small class="text-2xs text-muted">Read later as <code>values.{{ action.name }}</code>.</small>
          <small v-for="problem in problemsAt('/action/name')" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
        </label>
        <label class="grid gap-1.5">
          <span class="text-xs font-semibold">Kept</span>
          <select :value="action.scope" @change="patch({ scope: ($event.target as HTMLSelectElement).value as 'rule' | 'conversation', key: ($event.target as HTMLSelectElement).value === 'conversation' ? emptyValue() : null })">
            <option value="rule">With this rule</option>
            <option value="conversation">With a conversation</option>
          </select>
        </label>
      </div>
      <RuleValueInput
        v-if="action.scope === 'conversation'"
        :model-value="action.key ?? emptyValue()"
        label="Conversation key"
        :variables="variables"
        :problems="problemsAt('/action/key')"
        @update:model-value="key => patch({ key })"
      />
      <RuleValueInput
        :model-value="action.value"
        label="Value"
        expects="json"
        :variables="variables"
        :problems="problemsAt('/action/value')"
        @update:model-value="value => patch({ value })"
      />
    </template>

    <template v-else-if="action?.type === 'wait'">
      <div v-if="wait" class="flex flex-wrap items-end gap-2">
        <label class="grid min-w-24 flex-1 gap-1.5">
          <span class="text-xs font-semibold">Wait for</span>
          <input
            type="number"
            min="1"
            :value="wait.amount"
            @input="setWait(Number(($event.target as HTMLInputElement).value), wait!.unit)"
          >
        </label>
        <select
          class="w-32"
          :value="wait.unit"
          aria-label="Unit"
          @change="setWait(wait!.amount, Number(($event.target as HTMLSelectElement).value))"
        >
          <option v-for="unit in units" :key="unit.value" :value="unit.value">
            {{ unit.label }}
          </option>
        </select>
        <button type="button" class="basis-full rounded-full border border-line px-3 py-2 text-xs font-semibold text-accent" @click="useWaitFormula">
          Use a formula
        </button>
      </div>
      <RuleValueInput
        v-else
        :model-value="action.seconds"
        label="Seconds to wait"
        expects="number"
        :variables="variables"
        @update:model-value="seconds => setAction({ type: 'wait', seconds })"
      />
      <small v-for="problem in [...new Set(problemsAt('/action'))]" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
      <p class="m-0! text-xs text-muted">
        At most 7 days. The triggering then continues with the next steps; a rule keeps at most 20 triggerings waiting.
      </p>
    </template>

    <details v-if="mine.length" class="rounded-xl bg-coral/8 p-3">
      <summary class="cursor-pointer text-xs font-semibold text-coral">
        Review {{ mine.length }} validation {{ mine.length === 1 ? 'message' : 'messages' }} for this step
      </summary>
      <p v-for="(problem, index) in mine" :key="`${problem.path}:${index}`" class="m-0! mt-2! text-xs font-semibold text-coral">
        {{ problem.message }}
      </p>
    </details>
    <footer class="flex justify-between border-t border-line pt-3">
      <button type="button" class="inline-flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-coral hover:bg-coral/10" @click="emit('remove')">
        <Icon :name="Trash2" :size="15" />Remove this step
      </button>
    </footer>
  </section>
</template>
