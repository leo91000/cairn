<script setup lang="ts">
import type { UsageWindow } from '../../../../packages/contracts/accounts'
import { computed } from 'vue'
import { lowPercent, remainingPercent } from '../../../../packages/contracts/accounts'

// Usage left as concentric rings: the short window inside, the long one outside.
const props = withDefaults(defineProps<{
  windows: UsageWindow[]
  remaining: number | null
  muted?: boolean
  size?: number
}>(), { size: 112 })
const stroke = computed(() => Math.round(props.size / 11))
const rings = computed(() => props.windows.slice(0, 2).map((window, index) => {
  const radius = props.size / 2 - stroke.value / 2 - (props.windows.length > 1 && index === 0 ? stroke.value + 3 : 0)
  const circumference = 2 * Math.PI * radius
  const left = remainingPercent(window)
  return {
    id: window.id,
    radius,
    dash: `${circumference * left / 100} ${circumference}`,
    outer: index === 1 || props.windows.length === 1,
    low: left < lowPercent,
  }
}))
</script>

<template>
  <div class="relative shrink-0" :style="{ width: `${size}px`, height: `${size}px` }">
    <svg :width="size" :height="size" aria-hidden="true">
      <g v-for="ring in rings" :key="ring.id">
        <circle
          :cx="size / 2"
          :cy="size / 2"
          :r="ring.radius"
          fill="none"
          class="stroke-hover"
          :stroke-width="stroke"
        />
        <circle
          :cx="size / 2"
          :cy="size / 2"
          :r="ring.radius"
          fill="none"
          :stroke-width="stroke"
          stroke-linecap="round"
          :stroke-dasharray="ring.dash"
          :transform="`rotate(-90 ${size / 2} ${size / 2})`"
          class="transition-[stroke-dasharray] duration-700 motion-reduce:transition-none"
          :class="muted ? 'stroke-subtle opacity-60' : ring.low ? 'stroke-warning' : ring.outer ? 'stroke-accent/50' : 'stroke-accent'"
        />
      </g>
    </svg>
    <div class="absolute inset-0 grid place-items-center text-center">
      <div>
        <b class="block font-heading leading-none font-extrabold tracking-[-0.03em] tabular-nums" :style="{ fontSize: `${Math.round(size * 0.21)}px` }">{{ remaining === null ? '—' : `${Math.round(remaining)}%` }}</b>
        <small class="mt-0.5 block text-3xs text-muted">left</small>
      </div>
    </div>
  </div>
</template>
