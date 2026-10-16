<script setup lang="ts">
import type { SettingField } from '../plugins'
import { computed, useId } from 'vue'

// Renders a package's declarative fields. Secret values live in their own model and
// are never prefilled: a saved secret shows as a placeholder and stays when left blank.
const props = defineProps<{
  fields: SettingField[]
  saved?: string[]
  disabled?: boolean
}>()
const values = defineModel<Record<string, any>>('values', { required: true })
const secrets = defineModel<Record<string, string>>('secrets', { default: () => ({}) })
const uid = useId()
const described = (field: SettingField) => field.description ? `${uid}-${field.key}` : undefined

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
      <label v-if="field.type === 'boolean'" class="flex flex-row! items-center gap-2!">
        <input v-model="values[field.key]" type="checkbox" class="w-auto!">{{ field.title }}
      </label>
      <fieldset v-else-if="field.type === 'multiselect'" class="grid gap-1.5" :aria-label="field.title">
        <legend class="mb-1 text-xs font-medium">
          {{ field.title }}
        </legend>
        <label v-for="option in field.options" :key="option.value" class="flex flex-row! items-center gap-2! font-normal!">
          <input
            type="checkbox"
            class="w-auto!"
            :checked="(values[field.key] ?? []).includes(option.value)"
            @change="toggle(field.key, option.value, ($event.target as HTMLInputElement).checked)"
          >{{ option.label }}
        </label>
        <small v-if="field.description">{{ field.description }}</small>
      </fieldset>
      <label v-else>{{ field.title }}<select
        v-if="field.type === 'select'"
        v-model="values[field.key]"
        :required="field.required"
        :aria-describedby="described(field)"
      >
        <option v-if="!field.required" value="">—</option>
        <option v-for="option in field.options" :key="option.value" :value="option.value">{{ option.label }}</option>
      </select><textarea
        v-else-if="field.type === 'text'"
        v-model="values[field.key]"
        rows="4"
        :aria-describedby="described(field)"
        :required="field.required"
        :maxlength="field.maxLength"
        :placeholder="field.placeholder"
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
      ><input
        v-else
        v-model="values[field.key]"
        :type="field.type === 'number' ? 'number' : field.type === 'url' ? 'url' : 'text'"
        :aria-describedby="described(field)"
        :required="field.required"
        :maxlength="field.maxLength"
        :min="field.minimum"
        :max="field.maximum"
        :placeholder="field.placeholder"
      ></label>
      <small v-if="field.description && field.type !== 'boolean' && field.type !== 'multiselect'" :id="described(field)" class="-mt-3">{{ field.description }}</small>
    </template>
  </fieldset>
</template>
