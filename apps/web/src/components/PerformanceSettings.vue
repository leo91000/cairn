<script setup lang="ts">
import type { ClientPerformanceRuntime } from '../client-performance-runtime'
import { computed, ref } from 'vue'
import UiButton from './UiButton.vue'
import UiSwitch from './UiSwitch.vue'

const props = defineProps<{ performance: ClientPerformanceRuntime }>()
const report = ref(props.performance.snapshot())
const status = ref('')
const enabled = computed({ get: () => props.performance.enabled.value, set: value => props.performance.setEnabled(value) })
const serialized = computed(() => JSON.stringify(report.value, null, 2))
const canShare = typeof navigator.share === 'function'

function refresh() {
  report.value = props.performance.snapshot()
  status.value = ''
}

function reset() {
  props.performance.clear()
  refresh()
}

async function copy() {
  try {
    await navigator.clipboard.writeText(serialized.value)
    status.value = 'Report copied.'
  }
  catch { status.value = 'Copy unavailable. You can select the report below.' }
}

async function share() {
  try {
    await navigator.share({ title: 'Cairn performance report', text: serialized.value })
  }
  catch { /* Cancellation leaves the report available. */ }
}
</script>

<template>
  <section aria-label="Performance diagnostics">
    <h2 class="mb-4">
      Performance diagnostics
    </h2>
    <div class="flex items-center gap-3">
      <UiSwitch v-model="enabled" label="Send performance diagnostics" />
      <span>Send performance diagnostics</span>
    </div>
    <p class="mt-3 text-sm text-muted">
      Help identify slow screens and requests. Reports contain durations, fixed screen categories,
      connection type and this web build. They exclude messages, URLs, account details and device identifiers.
      Reports are private to your Cairn account and expire after 14 days.
    </p>
    <p class="mt-3 text-sm text-muted">
      Local measurements continue when sending is off. API times include connection recovery;
      browser responsiveness measures event-loop delays and long tasks where supported, rather than display frames.
      Measurements pause in the background. Reset starts a new local report; it does not delete reports already sent.
    </p>
    <div class="my-4 flex flex-wrap gap-2">
      <UiButton size="small" @click="refresh">
        Refresh report
      </UiButton>
      <UiButton size="small" @click="reset">
        Reset local report
      </UiButton>
      <UiButton size="small" :disabled="!report" @click="copy">
        Copy report
      </UiButton>
      <UiButton
        v-if="canShare"
        size="small"
        :disabled="!report"
        @click="share"
      >
        Share report
      </UiButton>
    </div>
    <p v-if="status" role="status" class="mb-3 text-sm text-muted">
      {{ status }}
    </p>
    <pre v-if="report" aria-label="Local performance report" class="max-h-96 overflow-auto rounded-lg border border-line p-3 text-xs">{{ serialized }}</pre>
    <p v-else class="text-sm text-muted">
      Performance reporting is unavailable in this browser context.
    </p>
  </section>
</template>
