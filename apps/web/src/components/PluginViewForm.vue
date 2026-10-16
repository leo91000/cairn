<script setup lang="ts">
import type { SettingField } from '../plugins'
import { ref } from 'vue'
import PluginFields from './PluginFields.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  node: { fields: SettingField[], submit: { label: string, action: string }, values?: Record<string, any> }
  busy?: boolean
}>()
const emit = defineEmits<{ submit: [action: string, values: Record<string, any>] }>()

const values = ref<Record<string, any>>({ ...props.node.values })
const secrets = ref<Record<string, string>>({})

function submit() {
  // A form's secret fields go to the package like its other values.
  emit('submit', props.node.submit.action, { ...values.value, ...secrets.value })
  secrets.value = {}
}
</script>

<template>
  <form class="grid gap-4 rounded-xl border border-line bg-inset p-4" @submit.prevent="submit">
    <PluginFields
      v-model:values="values"
      v-model:secrets="secrets"
      :fields="node.fields"
      :disabled="busy"
    />
    <div class="flex justify-end">
      <UiButton
        type="submit"
        size="small"
        variant="primary"
        :disabled="busy"
      >
        {{ node.submit.label }}
      </UiButton>
    </div>
  </form>
</template>
