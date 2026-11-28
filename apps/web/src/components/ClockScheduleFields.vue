<script setup lang="ts">
import type { HookRuleTrigger } from '../../../../packages/contracts/generated/typescript/HookRuleTrigger'
import { ref, watch } from 'vue'
import { localDateTime, scheduledTimestamp } from '../schedule-time'

type ClockTrigger = Extract<HookRuleTrigger, { type: 'clock' }>

const model = defineModel<ClockTrigger>({ required: true })
const mode = ref<'cron' | 'once'>(model.value.cron == null ? 'once' : 'cron')
const timezone = ref(model.value.timezone)
const cron = ref(model.value.cron ?? '0 9 * * *')
const date = ref('')
if (model.value.at != null) {
  try {
    date.value = localDateTime(model.value.at, timezone.value)
  }
  catch {
    date.value = localDateTime(model.value.at, 'UTC')
  }
}

watch([mode, timezone, cron, date], () => {
  model.value = mode.value === 'cron'
    ? { type: 'clock', cron: cron.value.trim(), timezone: timezone.value.trim() }
    : { type: 'clock', at: scheduledTimestamp(date.value, timezone.value.trim()), timezone: timezone.value.trim() }
})
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
  </div>
</template>
