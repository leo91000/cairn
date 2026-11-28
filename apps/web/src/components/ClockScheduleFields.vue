<script setup lang="ts">
import type { HookRuleTrigger } from '../../../../packages/contracts/generated/typescript/HookRuleTrigger'
import { ref, watch } from 'vue'
import { previewSchedule } from '../../../../packages/contracts/generated/previewSchedule'
import { api } from '../api'
import { localDateTime, scheduledTimestamp } from '../schedule-time'

type ClockTrigger = Extract<HookRuleTrigger, { type: 'clock' }>

const model = defineModel<ClockTrigger>({ required: true })
const mode = ref<'cron' | 'once'>(model.value.cron == null ? 'once' : 'cron')
const timezone = ref(model.value.timezone)
const cron = ref(model.value.cron ?? '0 9 * * *')
const date = ref('')
const occurrences = ref<string[]>([])
const previewError = ref('')
const previewing = ref(false)
let previewGeneration = 0
if (model.value.at != null) {
  try {
    date.value = localDateTime(model.value.at, timezone.value)
  }
  catch {
    // An invalid stored date or timezone leaves an editable empty date field.
  }
}

watch([mode, timezone, cron, date], () => {
  previewGeneration++
  occurrences.value = []
  previewError.value = ''
  previewing.value = false
  model.value = mode.value === 'cron'
    ? { type: 'clock', cron: cron.value.trim(), timezone: timezone.value.trim() }
    : { type: 'clock', at: scheduledTimestamp(date.value, timezone.value.trim()), timezone: timezone.value.trim() }
})

async function preview() {
  const generation = ++previewGeneration
  previewing.value = true
  occurrences.value = []
  previewError.value = ''
  try {
    const result = await previewSchedule(api, { cron: cron.value.trim(), timezone: timezone.value.trim(), count: 5 })
    if (generation === previewGeneration) {
      const format = new Intl.DateTimeFormat(undefined, {
        dateStyle: 'medium',
        timeStyle: 'short',
        timeZone: result.timezone,
      })
      occurrences.value = result.occurrences.map(at => format.format(at))
    }
  }
  catch (error) {
    if (generation === previewGeneration)
      previewError.value = (error as Error).message
  }
  finally {
    if (generation === previewGeneration)
      previewing.value = false
  }
}
</script>

<template>
  <div class="grid gap-3">
    <label>
      Schedule
      <select v-model="mode">
        <option value="cron">Recurring</option>
        <option value="once">One time</option>
      </select>
    </label>
    <label v-if="mode === 'cron'">
      Cron (five fields)
      <input v-model="cron" placeholder="0 9 * * *" required>
    </label>
    <label v-else>
      Date and time
      <input v-model="date" type="datetime-local" required>
    </label>
    <label>
      Time zone
      <input v-model="timezone" placeholder="Europe/Paris" required>
    </label>
    <div v-if="mode === 'cron'" class="grid justify-items-start gap-2">
      <UiButton
        type="button"
        size="small"
        :disabled="previewing || !cron.trim() || !timezone.trim()"
        @click="preview"
      >
        Preview next runs
      </UiButton>
      <p v-if="previewError" role="alert" class="m-0! text-sm text-coral">
        {{ previewError }}
      </p>
      <ul v-if="occurrences.length" aria-label="Upcoming runs" class="m-0! grid gap-1 pl-5 text-sm text-muted">
        <li v-for="(time, index) in occurrences" :key="index">
          {{ time }}
        </li>
      </ul>
    </div>
  </div>
</template>
