<script setup lang="ts">
import {
  computed,
  ref,
  useId,
  watch,
} from 'vue'
import { Plus, Trash2 } from '../icons'
import { resolveRuleSchema, ruleLiteralDefault } from '../rule-schema'
import Icon from './Icon.vue'

defineOptions({ name: 'RuleLiteralFields' })
const props = defineProps<{
  schema: any
  root?: any
  label: string
  depth?: number
}>()
const model = defineModel<unknown>({ required: true })
const fieldId = useId()
const resolved = computed(() => resolveRuleSchema(props.schema, props.root ?? props.schema))
const fields = computed(() => Object.entries<any>(resolved.value.properties ?? {}))
const object = computed<Record<string, unknown>>(() => model.value && typeof model.value === 'object' && !Array.isArray(model.value) ? model.value as Record<string, unknown> : {})
const array = computed<unknown[]>(() => Array.isArray(model.value) ? model.value : [])

const numberDraft = ref('')
let emittedNumber: unknown = Symbol('initial')
watch(() => model.value, (value) => {
  if (!Object.is(value, emittedNumber))
    numberDraft.value = value === null || value === undefined ? '' : String(value)
}, { immediate: true })

function inputNumber(raw: string) {
  numberDraft.value = raw
  emittedNumber = raw.trim() && Number.isFinite(Number(raw)) ? Number(raw) : raw
  model.value = emittedNumber
}

function setField(name: string, value: unknown) {
  model.value = { ...object.value, [name]: value }
}

function setItem(index: number, value: unknown) {
  model.value = array.value.map((held, at) => at === index ? value : held)
}
</script>

<template>
  <select
    v-if="resolved.enum?.length"
    :value="JSON.stringify(model)"
    :aria-label="label"
    @change="model = JSON.parse(($event.target as HTMLSelectElement).value)"
  >
    <option v-for="option in resolved.enum" :key="JSON.stringify(option)" :value="JSON.stringify(option)">
      {{ String(option) }}
    </option>
  </select>
  <div v-else-if="resolved.type === 'boolean'" class="flex gap-2">
    <button
      v-for="flag in [true, false]"
      :key="String(flag)"
      type="button"
      class="rounded-full border px-3 py-1.5 text-xs font-semibold"
      :class="model === flag ? 'border-ink bg-ink text-canvas' : 'border-line'"
      :aria-pressed="model === flag"
      @click="model = flag"
    >
      {{ flag ? 'Yes' : 'No' }}
    </button>
  </div>
  <div v-else-if="resolved.type === 'object' && (depth ?? 0) < 8" class="grid gap-3 rounded-xl border border-line p-3">
    <div
      v-for="([name, field], index) in fields"
      :key="name"
      role="group"
      :aria-labelledby="`${fieldId}-${index}`"
      class="grid gap-1.5"
    >
      <span :id="`${fieldId}-${index}`" class="text-xs font-semibold">{{ field.title ?? name }}</span>
      <RuleLiteralFields
        :model-value="object[name] ?? ruleLiteralDefault(resolveRuleSchema(field, root ?? schema))"
        :schema="field"
        :root="root ?? schema"
        :depth="(depth ?? 0) + 1"
        :label="`${label}: ${field.title ?? name}`"
        @update:model-value="value => setField(name, value)"
      />
      <small v-if="field.description" class="text-2xs text-muted">{{ field.description }}</small>
    </div>
  </div>
  <div v-else-if="resolved.type === 'array' && (depth ?? 0) < 8" class="grid gap-2">
    <div v-for="(item, index) in array" :key="index" class="flex items-start gap-2 rounded-xl border border-line p-3">
      <div class="min-w-0 flex-1">
        <RuleLiteralFields
          :model-value="item"
          :schema="resolved.items ?? {}"
          :root="root ?? schema"
          :depth="(depth ?? 0) + 1"
          :label="`${label} ${index + 1}`"
          @update:model-value="value => setItem(index, value)"
        />
      </div>
      <button
        type="button"
        class="grid size-8 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-coral"
        :aria-label="`Remove ${label} ${index + 1}`"
        @click="model = array.filter((_, at) => at !== index)"
      >
        <Icon :name="Trash2" :size="14" />
      </button>
    </div>
    <button type="button" class="inline-flex items-center gap-1.5 justify-self-start rounded-full border border-dashed border-line px-3 py-1.5 text-xs font-semibold text-muted hover:text-accent" @click="model = [...array, ruleLiteralDefault(resolveRuleSchema(resolved.items ?? {}, root ?? schema))]">
      <Icon :name="Plus" :size="13" />Add an item
    </button>
  </div>
  <textarea
    v-else-if="resolved.type === 'string' && (resolved.maxLength ?? 0) > 200"
    :value="typeof model === 'string' ? model : ''"
    :aria-label="label"
    rows="3"
    @input="model = ($event.target as HTMLTextAreaElement).value"
  />
  <input
    v-else-if="resolved.type === 'integer' || resolved.type === 'number'"
    :value="numberDraft"
    :aria-label="label"
    inputmode="decimal"
    @input="inputNumber(($event.target as HTMLInputElement).value)"
  >
  <input
    v-else
    :value="model === null ? '' : String(model ?? '')"
    :aria-label="label"
    :inputmode="resolved.type === 'integer' || resolved.type === 'number' ? 'decimal' : 'text'"
    @input="model = ($event.target as HTMLInputElement).value"
  >
</template>
