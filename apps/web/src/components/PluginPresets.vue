<script setup lang="ts">
import type { SetupPreset, SetupStep } from '../plugin-setup'
import type { SettingField } from '../plugins'
import { computed, useId } from 'vue'
import { matchingPreset, presetValues, riskLabels } from '../plugin-setup'
import { riskTones } from '../plugins'

// A step's switches as a few named choices, e.g. "Read only". Fine-tuned switches that
// match no preset leave none selected.
const props = defineProps<{
  step: SetupStep
  fields: SettingField[]
  disabled?: boolean
}>()
const emit = defineEmits<{ refresh: [keys: string[]] }>()
const values = defineModel<Record<string, any>>('values', { required: true })
const uid = useId()
const selected = computed(() => matchingPreset(props.step, props.fields, values.value))

function choose(preset: SetupPreset) {
  const next = presetValues(props.step, props.fields, preset)
  const changed = props.fields.filter(field => field.refresh && Object.hasOwn(next, field.key) && values.value[field.key] !== next[field.key])
  Object.assign(values.value, next)
  if (changed.length)
    emit('refresh', changed.map(field => field.key))
}
</script>

<template>
  <fieldset class="grid min-w-0 gap-1.5" :disabled="disabled" :aria-label="step.title">
    <label
      v-for="preset in step.presets"
      :key="preset.id"
      class="grid! grid-cols-[auto_1fr_auto] items-start gap-x-2.5! gap-y-0.5! rounded-xl border border-line px-3.5 py-2.5 font-normal!"
      :class="selected === preset.id ? 'border-accent bg-soft' : 'hover:bg-hover/40'"
    >
      <input
        type="radio"
        class="mt-0.5 w-auto!"
        :name="`${uid}-preset`"
        :value="preset.id"
        :checked="selected === preset.id"
        @change="choose(preset)"
      ><span class="text-xs font-semibold">{{ preset.title }}</span>
      <span v-if="preset.risk" class="rounded-full px-2 py-px text-3xs font-semibold" :class="riskTones[preset.risk]">{{ riskLabels[preset.risk] }}</span>
      <small v-if="preset.description" class="col-start-2 col-end-4">{{ preset.description }}</small>
    </label>
    <small v-if="!selected" class="text-muted">Custom permissions, set below.</small>
  </fieldset>
</template>
