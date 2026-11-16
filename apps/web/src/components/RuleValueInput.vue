<script setup lang="ts">
import type { RuleValue } from '../../../../packages/contracts/generated/typescript/RuleValue'
import type { VariableOption } from '../hook-rules'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import {
  Search,
  SquareFunction,
  Variable,
  X,
} from '../icons'
import { resolveRuleSchema, ruleLiteralDefault } from '../rule-schema'
import Icon from './Icon.vue'
import RuleLiteralFields from './RuleLiteralFields.vue'
import RuleResources from './RuleResources.vue'

// One value of a rule: text where {{…}} inserts a variable, a fixed value, or a CEL
// formula. Variables are offered from the place of the value in the rule.
const props = withDefaults(defineProps<{
  label: string
  variables: VariableOption[]
  // What the receiving field expects; numbers and booleans also offer fixed values.
  expects?: 'text' | 'number' | 'boolean' | 'json' | 'field' | 'formula'
  multiline?: boolean
  placeholder?: string
  problems?: string[]
  hint?: string
  hideLabel?: boolean
  required?: boolean
  schema?: any
  schemaRoot?: any
  resourceSource?: {
    pluginId: string
    instanceId: string
    resourceType: string
    agentId: string
  }
  resourceLabel?: string
}>(), { expects: 'text', placeholder: '', resourceLabel: 'Choose a resource…' })
const model = defineModel<RuleValue>({ required: true })

type Mode = 'template' | 'literal' | 'formula'

const mode = computed<Mode>(() => model.value.type)
const modes = computed(() => {
  const all: Array<{ value: Mode, label: string, title: string }> = [
    { value: 'template', label: 'Text', title: 'Text, with variables inserted' },
    { value: 'literal', label: 'Value', title: 'A fixed value' },
    { value: 'formula', label: 'ƒ', title: 'A CEL formula' },
  ]
  if (['field', 'formula'].includes(props.expects))
    return all.filter(entry => entry.value === 'formula')
  const schemaType = props.schema ? resolveRuleSchema(props.schema, props.schemaRoot ?? props.schema).type : null
  if (props.expects === 'number' || props.expects === 'boolean' || (schemaType && schemaType !== 'string'))
    return all.filter(entry => entry.value !== 'template')
  return all
})

const literalDraft = ref<{ value: RuleValue, text: string, expects: string } | null>(null)
const text = computed({
  get: () => {
    const value = model.value
    if (value.type === 'template')
      return value.text
    if (value.type === 'formula')
      return value.expression
    if (literalDraft.value?.value === value && literalDraft.value.expects === props.expects)
      return literalDraft.value.text
    return typeof value.value === 'string' && props.expects !== 'json' ? value.value : JSON.stringify(value.value ?? null)
  },
  set: (next: string) => {
    if (model.value.type === 'template') {
      model.value = { type: 'template', text: next }
    }
    else if (model.value.type === 'formula') {
      model.value = { type: 'formula', expression: next }
    }
    else {
      const value: RuleValue = { type: 'literal', value: parseLiteral(next) }
      literalDraft.value = { value, text: next, expects: props.expects }
      model.value = value
    }
  },
})

function parseLiteral(raw: string): unknown {
  if (props.expects === 'number') {
    const number = Number(raw)
    return raw.trim() === '' || Number.isNaN(number) ? raw : number
  }

  if (props.expects === 'json') {
    try {
      return JSON.parse(raw)
    }
    catch {
      return raw
    }
  }

  return raw
}

// Map the visible JSON string selection to its decoded text before inserting a variable.
function jsonStringCaret(encoded: string, at: number): number {
  try {
    if (typeof JSON.parse(encoded) !== 'string')
      return at
    for (let end = Math.min(at, encoded.length - 1); end >= 1; end--) {
      try {
        return JSON.parse(`${encoded.slice(0, end)}"`).length
      }
      catch { /* A caret inside an escape belongs before that character. */ }
    }
  }
  catch { return at }

  return 0
}

// A template that only inserts one formula becomes that formula; other text is quoted.
function unwrap(template: string) {
  const trimmed = template.trim()
  const inner = trimmed.startsWith('{{') && trimmed.endsWith('}}') ? trimmed.slice(2, -2) : null
  if (inner !== null && !inner.includes('{{'))
    return inner.trim()
  return JSON.stringify(template)
}

function switchMode(next: Mode) {
  if (next === mode.value)
    return
  const held = model.value
  const literalText = held.type === 'literal' && typeof held.value === 'string' ? held.value : null
  const current = next === 'template' && literalText !== null ? literalText : text.value
  if (next === 'template') {
    model.value = { type: 'template', text: mode.value === 'formula' && current ? `{{${current}}}` : current }
  }
  else if (next === 'formula') {
    model.value = { type: 'formula', expression: mode.value === 'template' ? unwrap(current) : JSON.stringify(model.value.type === 'literal' ? model.value.value ?? null : current) }
  }
  else {
    let value = props.expects === 'boolean' ? true : parseLiteral(current)
    if (props.schema) {
      const schema = resolveRuleSchema(props.schema, props.schemaRoot ?? props.schema)
      const kind = Array.isArray(value) ? 'array' : value === null ? 'null' : typeof value
      if (kind !== schema.type && !(schema.type === 'integer' && kind === 'number'))
        value = ruleLiteralDefault(schema, props.schemaRoot ?? props.schema)
    }

    model.value = { type: 'literal', value }
  }
}

// The template's text, split into plain runs and {{…}} insertions for highlighting.
const segments = computed(() => {
  if (mode.value !== 'template')
    return []
  const parts: Array<{ text: string, variable: boolean }> = []
  const pattern = /\{\{.*?\}\}/gs
  let last = 0
  for (const match of text.value.matchAll(pattern)) {
    if (match.index! > last)
      parts.push({ text: text.value.slice(last, match.index), variable: false })
    parts.push({ text: match[0], variable: true })
    last = match.index! + match[0].length
  }

  parts.push({ text: `${text.value.slice(last)} `, variable: false })
  return parts
})

const field = ref<HTMLTextAreaElement>()
const overlay = ref<HTMLDivElement>()

function syncScroll() {
  if (overlay.value && field.value)
    overlay.value.scrollTop = field.value.scrollTop
}

function grow() {
  const element = field.value
  if (!element)
    return
  element.style.height = 'auto'
  element.style.height = `${Math.min(element.scrollHeight, props.multiline ? 280 : 120)}px`
}

watch(text, () => nextTick(grow))
watch(field, () => nextTick(grow))

const picking = ref(false)
const query = ref('')
const pickerRoot = ref<HTMLDivElement>()
const groups = computed(() => {
  const term = query.value.trim().toLowerCase()
  const matching = props.variables.filter(option => !term || `${option.label} ${option.expression} ${option.description}`.toLowerCase().includes(term))
  const grouped = new Map<string, VariableOption[]>()
  for (const option of matching)
    grouped.set(option.group, [...grouped.get(option.group) ?? [], option])
  return [...grouped.entries()]
})

function insert(option: VariableOption) {
  const element = field.value
  const current = text.value
  const schemaType = props.schema ? resolveRuleSchema(props.schema, props.schemaRoot ?? props.schema).type : null
  const typedSchema = schemaType != null && schemaType !== 'string'
  const typedLiteral = model.value.type === 'literal' && typeof model.value.value !== 'string'
  const typed = ['number', 'boolean'].includes(props.expects) || typedSchema || typedLiteral
  const replace = props.expects === 'field' || (typed && mode.value !== 'formula')
  const nextMode = ['field', 'formula'].includes(props.expects) || replace ? 'formula' : mode.value === 'literal' ? 'template' : mode.value
  const insertion = nextMode === 'template' ? `{{${option.expression}}}` : option.expression
  const jsonString = model.value.type === 'literal' && props.expects === 'json' && typeof model.value.value === 'string'
  const content = jsonString && model.value.type === 'literal' ? model.value.value as string : current
  const selectionStart = element?.selectionStart ?? current.length
  const selectionEnd = element?.selectionEnd ?? current.length
  const start = replace ? 0 : jsonString ? jsonStringCaret(current, selectionStart) : selectionStart
  const end = replace ? content.length : jsonString ? jsonStringCaret(current, selectionEnd) : selectionEnd
  const value = `${content.slice(0, start)}${insertion}${content.slice(end)}`
  model.value = nextMode === 'formula' ? { type: 'formula', expression: value } : { type: 'template', text: value }
  picking.value = false
  query.value = ''
  void nextTick(() => {
    field.value?.focus()
    const caret = start + insertion.length
    field.value?.setSelectionRange(caret, caret)
  })
}

function outside(event: PointerEvent) {
  if (picking.value && !pickerRoot.value?.contains(event.target as Node))
    picking.value = false
}

document.addEventListener('pointerdown', outside)
onBeforeUnmount(() => document.removeEventListener('pointerdown', outside))

const expectedKind = computed(() => props.expects === 'text' ? 'string' : props.expects === 'number' || props.expects === 'boolean' ? props.expects : props.schema ? resolveRuleSchema(props.schema, props.schemaRoot ?? props.schema).type : null)
const typeMismatch = computed(() => {
  const expected = expectedKind.value
  if (!expected || model.value.type === 'formula')
    return false
  if (model.value.type === 'template')
    return expected !== 'string'
  const value = model.value.value
  const kind = Array.isArray(value) ? 'array' : value === null ? 'null' : typeof value
  return expected === 'integer' ? typeof value !== 'number' || !Number.isInteger(value) : kind !== expected
})
const expectedLabels: Record<string, string> = {
  array: 'list',
  object: 'object',
  integer: 'number',
  number: 'number',
  boolean: 'boolean',
  string: 'text',
}
const expectedLabel = computed(() => expectedLabels[expectedKind.value ?? ''] ?? 'value')

const invalid = computed(() => (props.problems?.length ?? 0) > 0)
const fieldLabel = computed(() => props.variables.find(option => option.expression === text.value)?.label)
</script>

<template>
  <div class="rule-value grid gap-1.5" :class="{ invalid }">
    <div class="flex min-h-5 items-center gap-2">
      <span :class="hideLabel ? 'sr-only' : 'text-xs font-semibold text-ink'">{{ label }}<span v-if="required" class="text-coral"> *</span></span>
      <span class="flex-1" />
      <div
        v-if="modes.length > 1"
        class="flex items-center rounded-full border border-line p-0.5"
        role="group"
        :aria-label="`How to fill ${label}`"
      >
        <button
          v-for="option in modes"
          :key="option.value"
          type="button"
          class="rounded-full px-2.5 py-0.5 text-2xs font-semibold"
          :class="mode === option.value ? 'bg-ink text-canvas' : 'text-muted hover:text-ink'"
          :title="option.title"
          :aria-pressed="mode === option.value"
          @click="switchMode(option.value)"
        >
          {{ option.label }}
        </button>
      </div>
    </div>
    <div ref="pickerRoot" class="relative">
      <div
        v-if="mode === 'literal' && schema"
        class="grid gap-2"
      >
        <RuleLiteralFields
          :model-value="model.type === 'literal' ? model.value : null"
          :schema="schema"
          :root="schemaRoot ?? schema"
          :label="label"
          @update:model-value="value => model = { type: 'literal', value }"
        />
      </div>
      <div
        v-else-if="mode === 'literal' && expects === 'boolean'"
        class="flex items-center gap-2"
      >
        <button
          v-for="choice in [true, false]"
          :key="String(choice)"
          type="button"
          class="h-9 rounded-full border px-4 text-sm font-semibold"
          :class="model.type === 'literal' && model.value === choice ? 'border-ink bg-ink text-canvas' : 'border-line hover:bg-hover'"
          @click="model = { type: 'literal', value: choice }"
        >
          {{ choice ? 'Yes' : 'No' }}
        </button>
      </div>
      <div
        v-else
        class="value-shell relative flex items-start rounded-xl border bg-surface focus-within:border-accent focus-within:ring-3 focus-within:ring-accent/15"
        :class="invalid ? 'border-coral' : 'border-line'"
      >
        <span v-if="mode === 'formula'" class="grid h-9 w-8 shrink-0 place-items-center text-accent" aria-hidden="true">
          <Icon :name="SquareFunction" :size="16" />
        </span>
        <div class="relative min-w-0 flex-1">
          <div
            v-if="mode === 'template'"
            ref="overlay"
            class="value-text pointer-events-none absolute inset-0 overflow-hidden whitespace-pre-wrap break-words text-transparent"
            aria-hidden="true"
          >
            <template v-for="(segment, index) in segments" :key="index">
              <mark v-if="segment.variable" class="rounded-md bg-accent/15 text-transparent ring-1 ring-accent/30">{{ segment.text }}</mark><span v-else>{{ segment.text }}</span>
            </template>
          </div>
          <textarea
            ref="field"
            v-model="text"
            rows="1"
            class="value-text relative block w-full resize-none border-0! bg-transparent! shadow-none! outline-none"
            :class="mode === 'formula' ? 'font-mono text-[13px]' : ''"
            :placeholder="mode === 'formula' ? (expects === 'field' ? 'payload.author.id' : 'payload.content.size() > 10') : placeholder"
            :aria-label="label"
            :aria-invalid="invalid || undefined"
            spellcheck="false"
            @scroll="syncScroll"
            @input="grow"
          />
        </div>
        <button
          v-if="variables.length"
          type="button"
          class="m-1 grid size-7 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-accent"
          :aria-label="`Insert a variable into ${label}`"
          :aria-expanded="picking"
          title="Insert a variable"
          @click="picking = !picking"
        >
          <Icon :name="Variable" :size="16" />
        </button>
      </div>
      <button
        v-if="mode === 'literal' && (schema || expects === 'boolean') && variables.length"
        type="button"
        class="mt-2 text-xs font-semibold text-accent"
        :aria-label="`Insert a variable into ${label}`"
        @click="picking = !picking"
      >
        Insert a variable
      </button>
      <div
        v-if="picking"
        class="absolute right-0 z-30 mt-1.5 w-80 max-w-[calc(100vw-40px)] overflow-hidden rounded-2xl border border-line bg-raised shadow-xl"
        role="dialog"
        :aria-label="`Variables for ${label}`"
      >
        <label class="flex items-center gap-2 border-b border-line px-3">
          <Icon :name="Search" :size="14" class="text-muted" />
          <input
            v-model="query"
            class="w-full border-0! bg-transparent! py-2.5! text-sm shadow-none! outline-none"
            placeholder="Search variables"
            aria-label="Search variables"
          >
          <button
            type="button"
            class="text-muted hover:text-ink"
            aria-label="Close"
            @click="picking = false"
          ><Icon :name="X" :size="14" /></button>
        </label>
        <div class="max-h-72 overflow-y-auto py-1 [scrollbar-width:thin]">
          <section v-for="[group, options] in groups" :key="group">
            <p class="m-0! px-3 pt-2.5 pb-1 text-2xs font-bold tracking-wide text-muted uppercase">
              {{ group }}
            </p>
            <button
              v-for="option in options"
              :key="option.expression"
              type="button"
              class="flex w-full items-start gap-2.5 px-3 py-2 text-left hover:bg-hover"
              @click="insert(option)"
            >
              <span class="mt-0.5 shrink-0 rounded-md bg-accent/12 px-1.5 py-0.5 font-mono text-2xs text-accent">{{ option.type }}</span>
              <span class="min-w-0 flex-1">
                <span class="block truncate text-sm font-semibold">{{ option.label }}</span>
                <span v-if="option.description" class="block text-xs text-muted">{{ option.description }}</span>
                <code v-if="option.example" class="mt-1 block break-all text-2xs text-accent">{{ option.example }}</code>
              </span>
            </button>
          </section>
          <p v-if="!groups.length" class="m-0! px-3 py-4 text-sm text-muted">
            No variable matches.
          </p>
        </div>
      </div>
    </div>
    <small v-if="typeMismatch" class="text-xs text-coral" role="alert">Use a fixed {{ expectedLabel }} or a formula returning one.</small>
    <small v-if="expects === 'field' && fieldLabel" class="text-xs text-muted">{{ fieldLabel }}</small>
    <small v-else-if="hint && !invalid" class="text-xs text-muted">{{ hint }}</small>
    <small
      v-for="problem in problems"
      :key="problem"
      class="text-xs font-semibold text-coral"
      role="alert"
    >{{ problem }}</small>
    <RuleResources
      v-if="resourceSource"
      v-bind="resourceSource"
      :model-value="model.type === 'literal' && typeof model.value === 'string' && model.value ? [{ resourceType: resourceSource.resourceType, id: model.value }] : []"
      :label="resourceLabel"
      @update:model-value="values => model = { type: 'literal', value: values[0]?.id ?? '' }"
    />
  </div>
</template>

<style scoped>
.value-text {
  min-height: 36px;
  padding: 8px 10px;
  font-size: 14px;
  line-height: 20px;
  font-family: inherit;
  letter-spacing: normal;
}
.value-text.font-mono {
  font-family: var(--font-mono, ui-monospace, monospace);
}
mark {
  padding: 0;
  margin: 0;
}
</style>
