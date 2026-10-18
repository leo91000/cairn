<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { notify } from '../api'
import { pluginOperation } from '../plugin-operations'
import PluginView from './PluginView.vue'
import UiAlert from './UiAlert.vue'

// The package's own panel for one instance: its view, then whatever its actions return.
const props = defineProps<{ plugin: string, instance: string, name: string }>()

const nodes = ref<any[] | null>(null)
const busy = ref(false)
const error = ref('')
const path = () => `/plugins/${props.plugin}/connections/${props.instance}`

async function load() {
  busy.value = true
  error.value = ''
  try {
    nodes.value = (await pluginOperation<{ view: any[] | null }>(`${path()}/view`, undefined, 'GET')).view
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function act(action: string, payload: unknown, values?: Record<string, any>) {
  busy.value = true
  error.value = ''
  try {
    const result = await pluginOperation<{ notice?: string, view?: any[] }>(`${path()}/actions`, { action, payload, values: values ?? {} })
    if (result.view)
      nodes.value = result.view
    if (result.notice)
      notify(result.notice)
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

onMounted(load)
</script>

<template>
  <section class="grid gap-3 border-t border-line pt-4" :aria-label="`${name} panel`" :aria-busy="busy">
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <p v-if="!nodes && busy" class="text-2xs text-muted" role="status">
      Loading…
    </p>
    <PluginView
      v-if="nodes"
      :nodes="nodes"
      :busy="busy"
      @action="act"
    />
  </section>
</template>
