<script setup lang="ts">
import type { HookRuleProblem } from '../../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { RuleCondition } from '../../../../packages/contracts/generated/typescript/RuleCondition'
import type { RuleOperator } from '../../../../packages/contracts/generated/typescript/RuleOperator'
import type { VariableOption } from '../hook-rules'
import { computed, ref } from 'vue'
import {
  binaryOperator,
  comparisonExpects,
  operatorTitles,
  typedComparisonRight,
} from '../hook-rules'
import {
  Plus,
  Trash2,
  UserCheck,
  X,
} from '../icons'
import Icon from './Icon.vue'
import RuleResources from './RuleResources.vue'
import RuleValueInput from './RuleValueInput.vue'

defineOptions({ name: 'RuleConditionEditor' })
// A condition as people read it: groups where all or any conditions must hold, each a
// comparison of an event field, a choice of people, or a formula. Groups nest.
const props = defineProps<{
  path: string
  variables: VariableOption[]
  problems: HookRuleProblem[]
  depth?: number
  peopleSource?: { pluginId: string, instanceId: string, resourceType: string }
}>()
const model = defineModel<RuleCondition>({ required: true })
const depth = computed(() => props.depth ?? 0)
const group = computed(() => model.value.type === 'all' || model.value.type === 'any' ? model.value : { type: 'all' as const, conditions: [model.value] })

const operators = Object.entries(operatorTitles) as Array<[RuleOperator, string]>
const adding = ref(false)

const authorField = computed(() => props.variables.find(option => /(?:^|\.)author(?:\.id|Id)?$/i.test(option.label))?.expression ?? 'payload.author')

function problemsAt(path: string) {
  return props.problems.filter(problem => problem.path === path || problem.path.startsWith(`${path}/`)).map(problem => problem.message)
}

function conditionsOf(condition: RuleCondition) {
  return condition.type === 'all' || condition.type === 'any' ? condition.conditions : [condition]
}

function setConditions(next: RuleCondition[]) {
  if (model.value.type === 'all' || model.value.type === 'any')
    model.value = { type: model.value.type, conditions: next }
  else
    model.value = next.length === 1 ? next[0] : { type: 'all', conditions: next }
}

function conditionPath(index: number) {
  return model.value.type === 'all' || model.value.type === 'any' ? `${props.path}/conditions/${index}` : props.path
}

function add(kind: 'compare' | 'authorIn' | 'formula' | 'group' | 'conversationExists' | 'authorStartedConversation' | 'authorQuota') {
  adding.value = false
  const field = props.variables.find(option => option.group === 'Event')?.expression ?? 'payload'
  let condition: RuleCondition
  switch (kind) {
    case 'compare': condition = {
      type: 'compare',
      left: { type: 'formula', expression: field },
      operator: 'equals',
      right: { type: 'template', text: '' },
    }
      break
    case 'authorIn': condition = { type: 'authorIn', author: { type: 'formula', expression: authorField.value }, people: [] }
      break
    case 'conversationExists': condition = { type: 'conversationExists', key: { type: 'template', text: '' } }
      break
    case 'authorStartedConversation': condition = { type: 'authorStartedConversation', author: { type: 'formula', expression: authorField.value }, key: { type: 'template', text: '' } }
      break
    case 'authorQuota': condition = {
      type: 'authorQuota',
      author: { type: 'formula', expression: authorField.value },
      limit: 5,
      windowMinutes: 60,
    }
      break
    case 'formula': condition = { type: 'formula', expression: '' }
      break
    default: condition = { type: 'any', conditions: [] }
  }

  setConditions([...conditionsOf(model.value), condition])
}

function update(index: number, next: RuleCondition) {
  const conditions = [...conditionsOf(model.value)]
  conditions[index] = next
  setConditions(conditions)
}

function remove(index: number) {
  setConditions(conditionsOf(model.value).filter((_, position) => position !== index))
}

function toggleKind() {
  if (group.value)
    model.value = { type: group.value.type === 'all' ? 'any' : 'all', conditions: group.value.conditions }
}

function setOperator(condition: Extract<RuleCondition, { type: 'compare' }>, index: number, operator: RuleOperator) {
  update(index, {
    ...condition,
    operator,
    right: binaryOperator(operator) ? typedComparisonRight(condition.right, comparisonExpects(condition.left, operator, props.variables)) : null,
  })
}

const personDraft = ref<Record<number, string>>({})

function addPerson(condition: Extract<RuleCondition, { type: 'authorIn' }>, index: number) {
  const id = (personDraft.value[index] ?? '').trim()
  if (!id || condition.people.some(person => person.id === id))
    return
  update(index, { ...condition, people: [...condition.people, { resourceType: condition.people[0]?.resourceType ?? 'person', id }] })
  personDraft.value[index] = ''
}
</script>

<template>
  <div v-if="group" class="condition-group grid gap-2 rounded-2xl border p-3" :class="depth ? 'border-dashed border-line bg-canvas/40' : 'border-line bg-surface'">
    <div class="flex flex-wrap items-center gap-2 text-sm">
      <span class="text-muted">{{ depth ? 'Group:' : 'Continue when' }}</span>
      <button
        type="button"
        class="rounded-full border border-accent/40 bg-accent/10 px-3 py-1 text-xs font-bold text-accent hover:bg-accent/15"
        :aria-label="`Match ${group.type === 'all' ? 'all' : 'any'} of the conditions; switch`"
        @click="toggleKind"
      >
        {{ group.type === 'all' ? 'ALL' : 'ANY' }}
      </button>
      <span class="text-muted">of these hold</span>
    </div>
    <p v-if="!group.conditions.length" class="m-0! rounded-xl bg-canvas/60 px-3 py-2.5 text-sm text-muted">
      {{ depth ? 'Add a condition to this group.' : 'No condition yet: every event continues.' }}
    </p>
    <div v-for="(condition, index) in group.conditions" :key="index" class="condition-row relative grid gap-2">
      <div v-if="index > 0" class="-my-1 pl-3 text-2xs font-bold tracking-wide text-muted uppercase">
        {{ group.type === 'all' ? 'and' : 'or' }}
      </div>
      <div class="grid gap-2 rounded-xl border border-line bg-raised p-3">
        <template v-if="condition.type === 'compare'">
          <div class="grid grid-cols-[minmax(0,1.3fr)_minmax(130px,0.8fr)] gap-2 phone:grid-cols-1">
            <RuleValueInput
              :model-value="condition.left"
              label="Field"
              expects="field"
              :variables="variables"
              :problems="problemsAt(`${conditionPath(index)}/left`)"
              @update:model-value="left => update(index, { ...condition, left, right: binaryOperator(condition.operator) ? typedComparisonRight(condition.right, comparisonExpects(left, condition.operator, variables)) : null })"
            />
            <label class="grid gap-1.5 self-start">
              <span class="text-xs font-semibold">Test</span>
              <select :value="condition.operator" class="h-9" @change="setOperator(condition, index, ($event.target as HTMLSelectElement).value as RuleOperator)">
                <option v-for="[operator, title] in operators" :key="operator" :value="operator">{{ title }}</option>
              </select>
            </label>
          </div>
          <RuleValueInput
            v-if="condition.right"
            :model-value="condition.right"
            :label="condition.operator === 'in' ? 'Values (a list)' : condition.operator === 'matches' ? 'Regular expression' : 'Value'"
            :expects="comparisonExpects(condition.left, condition.operator, variables)"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/right`)"
            :hint="condition.operator === 'in' ? 'For example [&quot;alice&quot;, &quot;bob&quot;].' : undefined"
            @update:model-value="right => update(index, { ...condition, right })"
          />
        </template>
        <template v-else-if="condition.type === 'authorIn'">
          <p class="m-0! flex items-center gap-2 text-sm font-semibold">
            <Icon :name="UserCheck" :size="16" class="text-accent" />The author is one of these people
          </p>
          <RuleValueInput
            :model-value="condition.author"
            label="Author field"
            expects="field"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/author`)"
            @update:model-value="author => update(index, { ...condition, author })"
          />
          <RuleResources
            v-if="peopleSource"
            v-bind="peopleSource"
            :model-value="condition.people"
            multiple
            label="Choose people…"
            @update:model-value="people => update(index, { ...condition, people })"
          />
          <details>
            <summary class="cursor-pointer text-xs text-muted">
              Exact identifiers (advanced)
            </summary>
            <div class="flex flex-wrap items-center gap-1.5">
              <span
                v-for="person in condition.people"
                :key="person.id"
                class="inline-flex items-center gap-1 rounded-full bg-accent/12 py-1 pr-1 pl-3 text-xs font-semibold text-accent"
              >
                {{ person.id }}
                <button
                  type="button"
                  class="grid size-5 place-items-center rounded-full hover:bg-accent/20"
                  :aria-label="`Remove ${person.id}`"
                  @click="update(index, { ...condition, people: condition.people.filter(entry => entry.id !== person.id) })"
                >
                  <Icon :name="X" :size="12" />
                </button>
              </span>
              <input
                v-model="personDraft[index]"
                class="h-8! min-w-40 flex-1 rounded-full! text-sm"
                placeholder="Add a person’s ID, then Enter"
                aria-label="Add a person"
                @keydown.enter.prevent="addPerson(condition, index)"
              >
            </div>
          </details>
          <small v-for="problem in problemsAt(`${conditionPath(index)}/people`)" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
        </template>
        <template v-else-if="condition.type === 'formula'">
          <RuleValueInput
            :model-value="{ type: 'formula', expression: condition.expression }"
            label="Formula (true or false)"
            expects="field"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/expression`)"
            @update:model-value="value => update(index, { type: 'formula', expression: value.type === 'formula' ? value.expression : '' })"
          />
        </template>
        <template v-else-if="condition.type === 'all' || condition.type === 'any'">
          <RuleConditionEditor
            :model-value="condition"
            :path="`${conditionPath(index)}`"
            :variables="variables"
            :problems="problems"
            :depth="depth + 1"
            :people-source="peopleSource"
            @update:model-value="next => update(index, next)"
          />
        </template>
        <template v-else-if="condition.type === 'conversationExists' || condition.type === 'authorStartedConversation'">
          <p class="m-0! text-sm font-semibold">
            {{ condition.type === 'conversationExists' ? 'A conversation exists for this key' : 'The author started the conversation for this key' }}
          </p>
          <RuleValueInput
            :model-value="condition.key"
            label="Conversation key"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/key`)"
            @update:model-value="key => update(index, { ...condition, key })"
          />
          <RuleValueInput
            v-if="condition.type === 'authorStartedConversation'"
            :model-value="condition.author"
            label="Author"
            expects="field"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/author`)"
            @update:model-value="author => update(index, { ...condition, author })"
          />
        </template>
        <template v-else-if="condition.type === 'authorQuota'">
          <p class="m-0! text-sm font-semibold">
            Limit matching events per author
          </p>
          <RuleValueInput
            :model-value="condition.author"
            label="Author"
            expects="field"
            :variables="variables"
            :problems="problemsAt(`${conditionPath(index)}/author`)"
            @update:model-value="author => update(index, { ...condition, author })"
          />
          <div class="grid grid-cols-2 gap-3">
            <label class="grid gap-1 text-xs font-semibold">At most<input
              type="number"
              min="1"
              :value="condition.limit"
              @input="update(index, { ...condition, limit: Number(($event.target as HTMLInputElement).value) })"
            ></label>
            <label class="grid gap-1 text-xs font-semibold">Window (minutes)<input
              type="number"
              min="1"
              :value="condition.windowMinutes"
              @input="update(index, { ...condition, windowMinutes: Number(($event.target as HTMLInputElement).value) })"
            ></label>
          </div>
        </template>
        <template v-else-if="condition.type === 'not'">
          <p class="m-0! text-xs font-semibold text-muted">
            This condition must NOT hold
          </p>
          <RuleConditionEditor
            :model-value="condition.condition"
            :path="`${conditionPath(index)}/condition`"
            :variables="variables"
            :problems="problems"
            :depth="depth + 1"
            :people-source="peopleSource"
            @update:model-value="next => update(index, { type: 'not', condition: next })"
          />
        </template>
        <p
          v-for="(problem, at) in problems.filter(problem => problem.path === conditionPath(index) || (condition.type === 'authorQuota' && [`${conditionPath(index)}/limit`, `${conditionPath(index)}/windowMinutes`].includes(problem.path)))"
          :key="`${problem.path}:${at}`"
          class="m-0! text-xs font-semibold text-coral"
          role="alert"
        >
          {{ problem.message }}
        </p>
        <div class="flex justify-end gap-2">
          <button type="button" class="rounded-lg px-2 py-1 text-xs text-muted hover:bg-hover" @click="update(index, condition.type === 'not' ? condition.condition : { type: 'not', condition })">
            {{ condition.type === 'not' ? 'Stop inverting' : 'Invert' }}
          </button>
          <button type="button" class="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-xs text-muted hover:bg-hover hover:text-coral" @click="remove(index)">
            <Icon :name="Trash2" :size="13" />Remove
          </button>
        </div>
      </div>
    </div>
    <p
      v-for="(problem, at) in problems.filter(problem => problem.path === path && (model.type === 'all' || model.type === 'any'))"
      :key="at"
      class="m-0! text-xs font-semibold text-coral"
      role="alert"
    >
      {{ problem.message }}
    </p>
    <small v-for="problem in problemsAt(`${path}/conditions`).filter((_, i) => i < 1 && !group?.conditions.length)" :key="problem" class="text-xs font-semibold text-coral">{{ problem }}</small>
    <div class="relative">
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-full border border-line px-3 py-1.5 text-xs font-semibold hover:bg-hover"
        :aria-expanded="adding"
        @click="adding = !adding"
      >
        <Icon :name="Plus" :size="14" />Add a condition
      </button>
      <div v-if="adding" class="absolute left-0 z-20 mt-1.5 grid w-72 overflow-hidden rounded-2xl border border-line bg-raised py-1 shadow-xl" role="menu">
        <button
          type="button"
          role="menuitem"
          class="px-3.5 py-2.5 text-left hover:bg-hover"
          @click="add('compare')"
        >
          <span class="block text-sm font-semibold">Compare a field</span><span class="block text-xs text-muted">The channel is #support, the text contains “help”…</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="px-3.5 py-2.5 text-left hover:bg-hover"
          @click="add('authorIn')"
        >
          <span class="block text-sm font-semibold">The author is one of…</span><span class="block text-xs text-muted">Only answer people you choose.</span>
        </button>
        <button
          type="button"
          role="menuitem"
          class="px-3.5 py-2.5 text-left hover:bg-hover"
          @click="add('formula')"
        >
          <span class="block text-sm font-semibold">A formula</span><span class="block text-xs text-muted">Any CEL expression that is true or false.</span>
        </button>
        <button
          v-for="choice in [{ kind: 'conversationExists' as const, title: 'A conversation exists' }, { kind: 'authorStartedConversation' as const, title: 'Author of a conversation' }, { kind: 'authorQuota' as const, title: 'Per-author quota' }]"
          :key="choice.kind"
          type="button"
          role="menuitem"
          class="px-3.5 py-2.5 text-left text-sm font-semibold hover:bg-hover"
          @click="add(choice.kind)"
        >
          {{ choice.title }}
        </button>
        <button
          v-if="depth < 4"
          type="button"
          role="menuitem"
          class="px-3.5 py-2.5 text-left hover:bg-hover"
          @click="add('group')"
        >
          <span class="block text-sm font-semibold">A group</span><span class="block text-xs text-muted">Combine conditions with AND / OR.</span>
        </button>
      </div>
    </div>
  </div>
</template>
