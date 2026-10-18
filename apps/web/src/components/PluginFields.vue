<script setup lang="ts">
import type { FieldOption, SettingField } from '../plugins'
import { computed, useId } from 'vue'
import { pluginFieldAnswer, pluginFormSecrets } from '../plugin-form-values'

// Renders a package's declarative fields. Secret values live in their own model and
// are never prefilled: a saved secret shows as a placeholder and stays when left blank.
// Dynamic choices and errors come from the package's form handler; fields marked
// `refresh` ask it again when they change.
const props = defineProps<{
  fields: SettingField[]
  saved?: string[]
  disabled?: boolean
  options?: Record<string, FieldOption[]>
  errors?: Record<string, string>
}>()
const emit = defineEmits<{ refresh: [key: string] }>()
const values = defineModel<Record<string, any>>('values', { required: true })
const secrets = defineModel<Record<string, string>>('secrets', { default: pluginFormSecrets })
const uid = useId()
const error = (field: SettingField) => pluginFieldAnswer(props.errors, field.key)
const described = (field: SettingField) => field.description || error(field) ? `${uid}-${field.key}` : undefined

const sections = computed(() => {
  const groups: { title: string, fields: SettingField[] }[] = []
  for (const field of props.fields.filter(field => !field.hidden)) {
    const title = field.section ?? ''
    const group = groups.find(group => group.title === title)
    if (group)
      group.fields.push(field)
    else
      groups.push({ title, fields: [field] })
  }

  return groups
})

function choices(field: SettingField) {
  return (field.dynamic ? pluginFieldAnswer(props.options, field.key) : undefined) ?? field.options ?? []
}

// Choices that explain themselves (e.g. why one can't be picked) read better as a list.
function explained(field: SettingField) {
  return choices(field).some(option => option.description || option.disabled)
}

function changed(field: SettingField) {
  if (field.refresh)
    emit('refresh', field.key)
}

function toggle(key: string, value: string, checked: boolean) {
  const current: string[] = Array.isArray(values.value[key]) ? values.value[key] : []
  values.value[key] = checked ? [...current, value] : current.filter(item => item !== value)
}
</script>

<template>
  <fieldset
    v-for="section in sections"
    :key="section.title"
    :disabled="disabled"
    class="grid min-w-0 gap-4"
  >
    <legend v-if="section.title" class="mb-1 text-xs font-semibold">
      {{ section.title }}
    </legend>
    <template v-for="field in section.fields" :key="field.key">
      <label v-if="field.type === 'boolean'" class="grid! grid-cols-[auto_1fr] items-start gap-x-2! gap-y-0.5!">
        <input
          v-model="values[field.key]"
          type="checkbox"
          class="mt-0.5 w-auto!"
          @change="changed(field)"
        >{{ field.title }}
        <small v-if="field.description" class="col-start-2 font-normal">{{ field.description }}</small>
      </label>
      <fieldset v-else-if="field.type === 'multiselect'" class="grid gap-1.5" :aria-label="field.title">
        <legend class="mb-1 text-xs font-medium">
          {{ field.title }}
        </legend>
        <label v-for="option in choices(field)" :key="option.value" class="flex flex-row! items-center gap-2! font-normal!">
          <input
            type="checkbox"
            class="w-auto!"
            :disabled="option.disabled"
            :checked="(values[field.key] ?? []).includes(option.value)"
            @change="toggle(field.key, option.value, ($event.target as HTMLInputElement).checked); changed(field)"
          >{{ option.label }}
        </label>
        <small v-if="field.description">{{ field.description }}</small>
      </fieldset>
      <fieldset v-else-if="field.type === 'select' && explained(field)" class="grid min-w-0 gap-1.5" :aria-describedby="described(field)">
        <legend class="mb-1 text-xs font-medium">
          {{ field.title }}
        </legend>
        <label
          v-for="option in choices(field)"
          :key="option.value"
          class="grid! grid-cols-[auto_1fr] items-start gap-x-2.5! gap-y-0.5! rounded-xl border border-line px-3.5 py-2.5 font-normal!"
          :class="option.disabled ? 'opacity-60' : values[field.key] === option.value ? 'border-accent bg-soft' : 'hover:bg-hover/40'"
        >
          <input
            v-model="values[field.key]"
            type="radio"
            class="mt-0.5 w-auto!"
            :name="`${uid}-${field.key}`"
            :value="option.value"
            :disabled="option.disabled"
            @change="changed(field)"
          ><span class="truncate text-xs font-semibold">{{ option.label }}</span>
          <small v-if="option.description" class="col-start-2">{{ option.description }}</small>
        </label>
        <small v-if="!choices(field).length" class="text-muted">Nothing to choose yet.</small>
      </fieldset>
      <label v-else>{{ field.title }}<select
        v-if="field.type === 'select'"
        v-model="values[field.key]"
        :required="field.required"
        :aria-describedby="described(field)"
        @change="changed(field)"
      >
        <option v-if="!field.required || field.dynamic" value="">—</option>
        <option
          v-for="option in choices(field)"
          :key="option.value"
          :value="option.value"
          :disabled="option.disabled"
        >{{ option.label }}</option>
      </select><textarea
        v-else-if="field.type === 'text'"
        v-model="values[field.key]"
        rows="4"
        :aria-describedby="described(field)"
        :required="field.required"
        :maxlength="field.maxLength"
        :placeholder="field.placeholder"
        @change="changed(field)"
      /><input
        v-else-if="field.type === 'secret'"
        v-model="secrets[field.key]"
        type="password"
        autocomplete="new-password"
        spellcheck="false"
        :aria-describedby="described(field)"
        :required="field.required && !saved?.includes(field.key)"
        :maxlength="field.maxLength"
        :placeholder="saved?.includes(field.key) ? 'Saved · leave blank to keep' : field.placeholder"
        @change="changed(field)"
      ><input
        v-else
        v-model="values[field.key]"
        :type="field.type === 'number' ? 'number' : field.type === 'url' ? 'url' : 'text'"
        :aria-describedby="described(field)"
        :required="field.required"
        :maxlength="field.maxLength"
        :min="field.minimum"
        :max="field.maximum"
        :step="field.type === 'number' ? 'any' : undefined"
        :placeholder="field.placeholder"
        @change="changed(field)"
      ></label>
      <small
        v-if="error(field) || (field.description && field.type !== 'boolean' && field.type !== 'multiselect')"
        :id="described(field)"
        class="-mt-3"
        :class="error(field) ? 'text-coral' : ''"
      >{{ error(field) || field.description }}</small>
    </template>
  </fieldset>
</template>
