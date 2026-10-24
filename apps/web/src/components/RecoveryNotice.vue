<script setup lang="ts">
import type { RetryState } from '../run-retry'
import { computed } from 'vue'
import { ChevronDown, RotateCw } from '../icons'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ recovery: RetryState, busy?: boolean }>()
defineEmits<{ cancel: [] }>()
const countdown = computed(() => {
  const seconds = props.recovery.seconds
  return seconds >= 60 ? `${Math.floor(seconds / 60)}:${(seconds % 60).toString().padStart(2, '0')}` : `${seconds}s`
})
</script>

<template>
  <section aria-label="Automatic recovery" class="my-3 min-w-0 overflow-hidden rounded-xl border border-accent/25 bg-surface shadow-sm" data-testid="recovery-notice">
    <div class="flex items-start gap-3 bg-accent/5 p-4 phone:gap-2.5 phone:p-3">
      <span class="grid size-9 shrink-0 place-items-center rounded-xl bg-accent/10 text-accent">
        <Icon :name="RotateCw" :size="19" :class="!recovery.seconds ? 'animate-spin motion-reduce:animate-none' : ''" />
      </span>
      <div class="min-w-0 flex-1">
        <p role="status" class="font-semibold leading-snug text-ink">
          {{ recovery.startup ? 'Reconnecting to your agent' : 'Your agent will try again' }}
        </p>
        <p class="mt-1 text-xs leading-relaxed text-muted">
          A temporary interruption. Your conversation and files are preserved.
        </p>
      </div>
      <div role="timer" aria-live="off" class="shrink-0 text-right text-accent">
        <p class="font-heading text-2xl font-semibold leading-none tabular-nums phone:text-xl">
          {{ recovery.seconds ? countdown : 'Soon' }}
        </p>
        <p class="mt-1.5 text-[10px] font-medium text-muted">
          {{ recovery.seconds ? 'until retry' : 'preparing retry' }}
        </p>
      </div>
    </div>
    <div class="px-4 pt-3 pb-2 phone:px-3">
      <div class="flex items-center gap-3">
        <div
          role="progressbar"
          aria-label="Automatic retry attempts"
          :aria-valuenow="recovery.attempt"
          :aria-valuemax="recovery.limit"
          :aria-valuemin="0"
          class="flex flex-1 gap-1.5"
        >
          <span
            v-for="step in recovery.limit"
            :key="step"
            class="h-1.5 flex-1 rounded-full"
            :class="step < recovery.attempt ? 'bg-accent/40' : step === recovery.attempt ? 'bg-accent' : 'bg-soft'"
          />
        </div>
        <span class="shrink-0 text-xs font-medium tabular-nums text-muted">Retry {{ recovery.attempt }} of {{ recovery.limit }}</span>
      </div>
      <div class="mt-2 flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
        <details class="group min-w-0 max-w-full flex-1 text-xs text-muted">
          <summary class="inline-flex cursor-pointer list-none items-center gap-1 py-2 hover:text-ink">
            What happened <Icon :name="ChevronDown" :size="12" class="group-open:rotate-180" />
          </summary>
          <p class="max-h-32 overflow-y-auto pb-2 leading-relaxed wrap-anywhere">
            {{ recovery.cause }}
          </p>
        </details>
        <UiButton size="small" :disabled="busy" @click="$emit('cancel')">
          Cancel retry
        </UiButton>
      </div>
    </div>
  </section>
</template>
