<script setup lang="ts">
import type { JsonDrafts } from '../rule-json-drafts'
import {
  computed,
  ref,
  toRaw,
  useId,
  watch,
} from 'vue'
import { Plus, Trash2 } from '../icons'
import {
  childJsonDrafts,
  jsonInputDraft,
  parseRuleJsonInput,
  removeJsonDraftItem,
  replaceChildJsonDrafts,
} from '../rule-json-drafts'
import {
  resolveRuleSchema,
  ruleLiteralConcreteDefault,
  ruleLiteralDefault,
  ruleSchemaAllowsNull,
  ruleSchemaUsesJson,
} from '../rule-schema'
import Icon from './Icon.vue'

defineOptions({ name: 'RuleLiteralFields' })
const props = withDefaults(defineProps<{
  modelValue: unknown
  jsonDrafts?: JsonDrafts
  schema: any
  root?: any
  label: string
  depth?: number
}>(), { jsonDrafts: () => ({}) })
const emit = defineEmits<{ change: [value: unknown, drafts: JsonDrafts] }>()
const model = computed(() => props.modelValue)

function publish(value: unknown, drafts: JsonDrafts = {}) {
  emit('change', value, drafts)
}

const fieldId = useId()
const textField = ref<HTMLInputElement | HTMLTextAreaElement>()
defineExpose({ field: textField })
const resolved = computed(() => resolveRuleSchema(props.schema, props.root ?? props.schema))
const nullable = computed(() => ruleSchemaAllowsNull(resolved.value, props.root ?? props.schema))
const genericJson = computed(() => ruleSchemaUsesJson(resolved.value, props.root ?? props.schema))
const concrete = computed(() => ruleLiteralConcreteDefault(resolved.value, props.root ?? props.schema))
const fields = computed(() => Object.entries<any>(resolved.value.properties ?? {}))
const required = computed<string[]>(() => resolved.value.required ?? [])
const object = computed<Record<string, unknown>>(() => model.value && typeof model.value === 'object' && !Array.isArray(model.value) ? model.value as Record<string, unknown> : {})
const array = computed<unknown[]>(() => Array.isArray(model.value) ? model.value : [])

const numberDraft = ref('')
let emittedNumber: unknown = Symbol('initial')
watch(() => [model.value, props.jsonDrafts['']?.text] as const, ([value, raw]) => {
  if (raw !== undefined)
    numberDraft.value = raw
  else if (!Object.is(value, emittedNumber))
    numberDraft.value = value === null || value === undefined ? '' : String(value)
}, { immediate: true })

function inputNumber(raw: string) {
  numberDraft.value = raw
  emittedNumber = raw.trim() && Number.isFinite(Number(raw)) ? Number(raw) : raw
  const valid = typeof emittedNumber === 'number' && Number.isFinite(emittedNumber)
    && (resolved.value.type !== 'integer' || Number.isInteger(emittedNumber))
  publish(emittedNumber, valid ? {} : jsonInputDraft(raw, resolved.value.type === 'integer' ? 'Enter a whole number.' : 'Enter a number.'))
}

const untyped = computed(() => genericJson.value || !resolved.value.type)
const jsonNoun = computed(() => resolved.value.type === 'array' ? 'list' : 'object')
const jsonDraft = ref('')
let emittedJson: unknown = Symbol('initial')
watch(() => [model.value, props.jsonDrafts['']?.text] as const, ([value, raw]) => {
  if (raw !== undefined)
    jsonDraft.value = raw
  else if (!Object.is(toRaw(value), emittedJson))
    jsonDraft.value = JSON.stringify(value ?? null, null, 2)
}, { immediate: true })

function matchesJson(value: unknown) {
  return untyped.value || (resolved.value.type === 'array' ? Array.isArray(value) : !!value && typeof value === 'object' && !Array.isArray(value))
}

function inputJson(raw: string) {
  jsonDraft.value = raw
  let drafts: JsonDrafts = {}
  try {
    const value = parseRuleJsonInput(raw)
    emittedJson = value
  }
  catch {
    emittedJson = raw
    drafts = jsonInputDraft(raw)
  }

  publish(emittedJson, drafts)
}

function setField(name: string, value: unknown, drafts: JsonDrafts) {
  publish({ ...object.value, [name]: value }, replaceChildJsonDrafts(props.jsonDrafts, name, drafts))
}

function setItem(index: number, value: unknown, drafts: JsonDrafts) {
  publish(array.value.map((held, at) => at === index ? value : held), replaceChildJsonDrafts(props.jsonDrafts, index, drafts))
}

function removeField(name: string) {
  publish(Object.fromEntries(Object.entries(object.value).filter(([key]) => key !== name)), replaceChildJsonDrafts(props.jsonDrafts, name, {}))
}
</script>

<template>
  <div v-if="model === null" class="grid gap-2 rounded-xl border border-line p-3">
    <span class="text-xs font-semibold">No value (null)</span>
    <button
      v-if="concrete !== undefined"
      type="button"
      class="justify-self-start rounded-full border border-line px-3 py-1.5 text-xs font-semibold"
      :aria-label="`Set a value for ${label}`"
      @click="publish(concrete)"
    >
      Set a value
    </button>
  </div>
  <select
    v-else-if="resolved.enum?.length"
    :value="JSON.stringify(model)"
    :aria-label="label"
    @change="publish(JSON.parse(($event.target as HTMLSelectElement).value))"
  >
    <option v-for="option in resolved.enum" :key="JSON.stringify(option)" :value="JSON.stringify(option)">
      {{ String(option) }}
    </option>
  </select>
  <div v-else-if="resolved.type === 'boolean' && !genericJson" class="flex gap-2">
    <button
      v-for="flag in [true, false]"
      :key="String(flag)"
      type="button"
      class="rounded-full border px-3 py-1.5 text-xs font-semibold"
      :class="model === flag ? 'border-ink bg-ink text-canvas' : 'border-line'"
      :aria-pressed="model === flag"
      @click="publish(flag)"
    >
      {{ flag ? 'Yes' : 'No' }}
    </button>
  </div>
  <div v-else-if="resolved.type === 'object' && !genericJson && model && typeof model === 'object' && !Array.isArray(model) && fields.length && (depth ?? 0) < 8" class="grid gap-3 rounded-xl border border-line p-3">
    <div
      v-for="([name, field], index) in fields"
      :key="name"
      role="group"
      :aria-labelledby="`${fieldId}-${index}`"
      class="grid gap-1.5"
    >
      <div class="flex items-center justify-between gap-2">
        <span :id="`${fieldId}-${index}`" class="text-xs font-semibold">{{ field.title ?? name }}<span v-if="required.includes(name)"> *</span></span>
        <button
          v-if="Object.hasOwn(object, name) && !required.includes(name)"
          type="button"
          class="text-xs text-muted hover:text-coral"
          :aria-label="`Remove field ${field.title ?? name}`"
          @click="removeField(name)"
        >
          Remove
        </button>
      </div>
      <RuleLiteralFields
        v-if="Object.hasOwn(object, name)"
        :model-value="object[name]"
        :json-drafts="childJsonDrafts(jsonDrafts, name)"
        :schema="field"
        :root="root ?? schema"
        :depth="(depth ?? 0) + 1"
        :label="`${label}: ${field.title ?? name}`"
        @change="(value, drafts) => setField(name, value, drafts)"
      />
      <div v-else class="grid gap-1.5">
        <small class="text-xs text-muted">{{ required.includes(name) ? 'Required field, not set.' : 'Not set.' }}</small>
        <button
          type="button"
          class="justify-self-start rounded-full border border-dashed border-line px-3 py-1.5 text-xs font-semibold"
          :aria-label="`Add field ${field.title ?? name}`"
          @click="setField(name, ruleLiteralDefault(field, root ?? schema), {})"
        >
          Add this field
        </button>
      </div>
      <small v-if="field.description" class="text-2xs text-muted">{{ field.description }}</small>
    </div>
  </div>
  <div v-else-if="resolved.type === 'array' && !genericJson && Array.isArray(model) && (depth ?? 0) < 8" class="grid gap-2">
    <div v-for="(item, index) in array" :key="index" class="flex items-start gap-2 rounded-xl border border-line p-3">
      <div class="min-w-0 flex-1">
        <RuleLiteralFields
          :model-value="item"
          :json-drafts="childJsonDrafts(jsonDrafts, index)"
          :schema="resolved.items ?? {}"
          :root="root ?? schema"
          :depth="(depth ?? 0) + 1"
          :label="`${label} ${index + 1}`"
          @change="(value, drafts) => setItem(index, value, drafts)"
        />
      </div>
      <button
        type="button"
        class="grid size-8 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-coral"
        :aria-label="`Remove ${label} ${index + 1}`"
        @click="publish(array.filter((_, at) => at !== index), removeJsonDraftItem(jsonDrafts, index))"
      >
        <Icon :name="Trash2" :size="14" />
      </button>
    </div>
    <button type="button" class="inline-flex items-center gap-1.5 justify-self-start rounded-full border border-dashed border-line px-3 py-1.5 text-xs font-semibold text-muted hover:text-accent" @click="publish([...array, ruleLiteralDefault(resolved.items ?? {}, root ?? schema)], jsonDrafts)">
      <Icon :name="Plus" :size="13" />Add an item
    </button>
  </div>
  <div v-else-if="resolved.type === 'object' || resolved.type === 'array' || untyped" class="grid gap-1.5">
    <textarea
      ref="textField"
      :value="jsonDraft"
      :aria-invalid="Object.hasOwn(jsonDrafts, '') || !matchesJson(model) || undefined"
      :aria-label="`${label} (${untyped ? 'JSON' : `JSON ${jsonNoun}`})`"
      rows="4"
      spellcheck="false"
      @input="inputJson(($event.target as HTMLTextAreaElement).value)"
    />
    <small class="text-xs text-muted">{{ untyped ? 'Enter a JSON value: text in quotes, number, boolean, object or list.' : resolved.type === 'array' ? "Enter the list's values as JSON." : "Enter the object's key/value properties as JSON." }}</small>
    <small v-if="Object.hasOwn(jsonDrafts, '') || !matchesJson(model)" class="text-xs text-coral" role="alert">Enter a valid JSON{{ untyped ? ' value' : ` ${jsonNoun}` }}.</small>
  </div>
  <textarea
    v-else-if="resolved.type === 'string' && (resolved.maxLength ?? 0) > 200"
    ref="textField"
    :value="typeof model === 'string' ? model : ''"
    :aria-label="label"
    rows="3"
    @input="publish(($event.target as HTMLTextAreaElement).value)"
  />
  <input
    v-else-if="resolved.type === 'integer' || resolved.type === 'number'"
    ref="textField"
    :value="numberDraft"
    :aria-label="label"
    inputmode="decimal"
    @input="inputNumber(($event.target as HTMLInputElement).value)"
  >
  <input
    v-else
    ref="textField"
    :value="model === null ? '' : String(model ?? '')"
    :aria-label="label"
    :inputmode="resolved.type === 'integer' || resolved.type === 'number' ? 'decimal' : 'text'"
    @input="publish(($event.target as HTMLInputElement).value)"
  >
  <button
    v-if="model !== null && nullable && !resolved.enum?.length"
    type="button"
    class="justify-self-start text-xs text-muted underline decoration-dotted underline-offset-2"
    :aria-label="`Use no value for ${label}`"
    @click="publish(null)"
  >
    Use no value (null)
  </button>
  <small v-if="jsonDrafts[''] && ['integer', 'number'].includes(resolved.type)" class="text-xs text-coral" role="alert">{{ jsonDrafts[''].message }}</small>
</template>
