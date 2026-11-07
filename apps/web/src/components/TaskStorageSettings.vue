<script setup lang="ts">
import type { TaskStorageLimits } from '../../../../packages/contracts/generated/typescript/TaskStorageLimits'
import { computed, onMounted, ref } from 'vue'
import { getTaskStorageLimits } from '../../../../packages/contracts/generated/getTaskStorageLimits'
import { saveTaskStorageLimits } from '../../../../packages/contracts/generated/saveTaskStorageLimits'
import { api, notify } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const current = ref<TaskStorageLimits>()
const maximum = ref<number | string>(150)
const busy = ref(false)
const error = ref('')
const number = computed(() => Number(maximum.value))
const valid = computed(() => maximum.value !== '' && Number.isInteger(number.value) && number.value >= 32 && number.value <= 1024)
const changed = computed(() => current.value && number.value !== current.value.maxDiskGiB)

async function load() {
  busy.value = true
  error.value = ''
  try {
    current.value = await getTaskStorageLimits(api, {})
    maximum.value = current.value.maxDiskGiB
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}

async function save() {
  if (!current.value || !valid.value || !changed.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    current.value = await saveTaskStorageLimits(api, { ...current.value, maxDiskGiB: number.value })
    maximum.value = current.value.maxDiskGiB
    notify('Task storage limit saved')
  }
  catch (cause) {
    error.value = (cause as Error).message
  }
  finally {
    busy.value = false
  }
}

onMounted(load)
</script>

<template>
  <section class="panel border-b border-line settings-section mb-5.5 px-0 py-7 phone:py-5">
    <h2>Storage per task</h2>
    <p class="text-sm text-muted my-3">
      Each conversation or mission run starts with up to 32 GiB. Native workspaces can grow as needed, up to this installation-wide limit. The workspace and managed temporary files count together.
    </p>
    <p class="text-sm text-muted my-3">
      Changes limit future growth. Space already granted to running or saved workspaces is preserved, and existing larger workspaces cannot grow further. No files are deleted.
    </p>
    <UiAlert v-if="error" class="my-3">
      {{ error }}
      <UiButton class="mt-3" :disabled="busy" @click="load">
        Reload saved limit
      </UiButton>
    </UiAlert>
    <form v-if="current" class="grid gap-4 max-w-xl" @submit.prevent="save">
      <label>Maximum disk per task (GiB)<input
        v-model="maximum"
        type="number"
        min="32"
        max="1024"
        step="1"
        required
        :disabled="busy"
      ></label>
      <p class="text-xs text-muted">
        Choose 32–1,024 GiB. New installations default to 150 GiB. Node budgets and host reserves still apply.
      </p>
      <UiButton type="submit" :disabled="busy || !valid || !changed">
        {{ busy ? 'Saving…' : 'Save task storage limit' }}
      </UiButton>
    </form>
    <p v-else-if="busy" role="status" class="text-sm text-muted my-3">
      Loading task storage limit…
    </p>
  </section>
</template>
