<script setup lang="ts">
import type { PluginFormLookup } from '../plugin-form-lookups'
import type { FieldOption, SettingField } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { choicePageStatus } from '../plugin-form-lookups'
import { pluginFieldAnswer } from '../plugin-form-values'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ field: SettingField, lookup?: PluginFormLookup, disabled?: boolean }>()
const emit = defineEmits<{ change: [] }>()
const model = defineModel<any>({ required: true })
const query = ref('')
const options = ref<FieldOption[]>([])
const labels = ref(new Map<string, FieldOption>())
const next = ref<string | null>()
const loading = ref(false)
const error = ref('')
const selected = computed<string[]>(() => props.field.type === 'multiselect' ? Array.isArray(model.value) ? model.value : [] : model.value ? [model.value] : [])
const selectedOption = (id: string) => labels.value.get(id) ?? { value: id, label: id }
const status = computed(() => choicePageStatus(options.value.length, next.value))
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
let cursor: string | undefined

function remember() {
  // Only selected names survive a page change; the result window never grows.
  labels.value = new Map(selected.value.slice(0, 100).map((id) => {
    const option = options.value.find(option => option.value === id) ?? selectedOption(id)
    return [id, option]
  }))
}

async function load(page?: string) {
  const request = ++generation
  cursor = page
  loading.value = true
  options.value = []
  next.value = undefined
  error.value = ''
  try {
    if (!props.lookup)
      throw new Error('This form does not support remote choices.')
    const result = await props.lookup.request({ field: props.field.key, query: query.value, ...(page && { cursor: page }) })
    if (request !== generation)
      return
    const more = pluginFieldAnswer(result.pages, props.field.key)?.nextCursor
    if (page && more === page)
      throw new Error('The plugin returned the same page cursor. Retry or change the search.')
    options.value = pluginFieldAnswer(result.options, props.field.key) ?? []
    next.value = more
    remember()
    error.value = Object.values(result.errors).join(' ')
  }
  catch (e) {
    if (request === generation)
      error.value = (e as Error).message
  }
  finally {
    if (request === generation)
      loading.value = false
  }
}

function search(value: string, delay = 250) {
  generation++
  clearTimeout(timer)
  query.value = value
  options.value = []
  next.value = undefined
  error.value = ''
  loading.value = !props.disabled
  if (!props.disabled)
    timer = setTimeout(() => void load(), delay)
}

function choose(value: string | string[]) {
  model.value = value
  // Capture the current choice before a refresh replaces this page.
  remember()
  emit('change')
}

function toggle(id: string, checked: boolean) {
  choose(checked ? [...selected.value, id] : selected.value.filter(value => value !== id))
}

watch(() => JSON.stringify({ field: props.field, scope: props.lookup?.context(props.field.key) }), () => {
  labels.value = new Map()
  search('', 0)
}, { immediate: true, flush: 'sync' })
watch(() => JSON.stringify(selected.value), () => {
  generation++
  clearTimeout(timer)
  loading.value = false
  remember()
}, { flush: 'sync' })
watch(() => props.disabled, (disabled) => {
  if (disabled) {
    generation++
    clearTimeout(timer)
    loading.value = false
  }
  else {
    search(query.value, 0)
  }
})
onBeforeUnmount(() => {
  generation++
  clearTimeout(timer)
})
</script>

<template>
  <!-- Search text is not a setting edit and must not refresh the full form. -->
  <div class="grid min-w-0 gap-2" @input.stop @change.stop>
    <VirtualSelect
      v-if="field.type === 'select'"
      :model-value="model || ''"
      :label="field.title"
      :options="options"
      :selected-option="model ? selectedOption(model) : undefined"
      :required="field.required"
      :clearable="!field.required"
      :disabled="disabled"
      :loading="loading"
      :page-status="status"
      :next-page="!!next"
      remote
      @search="search"
      @next="load(next!)"
      @update:model-value="choose"
    />
    <fieldset
      v-else
      class="grid gap-2"
      :disabled="disabled"
      :aria-label="field.title"
    >
      <legend class="text-xs font-medium">
        {{ field.title }}
      </legend>
      <label>Search {{ field.title.toLowerCase() }}<input
        :value="query"
        maxlength="200"
        type="search"
        @input="search(($event.target as HTMLInputElement).value)"
      ></label>
      <span v-if="loading" role="status" class="text-xs text-muted">Loading choices…</span>
      <label v-for="option in options" :key="option.value" class="flex flex-row! items-center gap-2! font-normal!">
        <input
          type="checkbox"
          class="w-auto!"
          :checked="selected.includes(option.value)"
          :disabled="loading || option.disabled || (!selected.includes(option.value) && selected.length >= 100)"
          @change="toggle(option.value, ($event.target as HTMLInputElement).checked)"
        >
        <span>{{ option.label }}<small v-if="option.description" class="block">{{ option.description }}</small></span>
      </label>
      <small role="status">{{ status }}</small>
      <UiButton
        v-if="next"
        type="button"
        :disabled="loading"
        @click="load(next!)"
      >
        Next page
      </UiButton>
      <div v-if="selected.length" class="grid gap-1" aria-label="Selected choices">
        <span v-for="id in selected" :key="id" class="flex items-center justify-between gap-2 text-xs">
          {{ selectedOption(id).label }}<UiButton type="button" size="small" @click="toggle(id, false)">Remove {{ selectedOption(id).label }}</UiButton>
        </span>
      </div>
    </fieldset>
    <div v-if="error" role="alert" class="text-xs text-coral">
      {{ error }}
    </div>
    <UiButton
      v-if="error || next === undefined"
      type="button"
      size="small"
      :disabled="disabled || loading"
      @click="load(cursor)"
    >
      Retry choices
    </UiButton>
  </div>
</template>
