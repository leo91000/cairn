<script setup lang="ts">
import BrandWordmark from './BrandWordmark.vue'
import ThemeControl from './ThemeControl.vue'

withDefaults(defineProps<{
  maxWidth?: 'sm' | 'md'
  embedded?: boolean
  label?: string
  busy?: boolean
}>(), {
  maxWidth: 'sm',
  embedded: false,
  label: undefined,
  busy: false,
})
</script>

<template>
  <component
    :is="embedded ? 'div' : 'main'"
    class="grid place-items-center px-5 py-10 text-ink phone:px-4"
    :class="embedded ? 'min-h-[60dvh]' : 'auth-stage min-h-dvh pt-17 phone:pt-14'"
  >
    <svg
      v-if="!embedded"
      class="pointer-events-none absolute -top-28 left-1/2 w-[780px] max-w-none -translate-x-1/2 text-ink opacity-[0.055]"
      viewBox="0 0 800 800"
      fill="none"
      aria-hidden="true"
    >
      <g stroke="currentColor" stroke-width="1">
        <ellipse
          cx="400"
          cy="360"
          rx="140"
          ry="90"
          transform="rotate(-14 400 360)"
        />
        <ellipse
          cx="400"
          cy="360"
          rx="220"
          ry="145"
          transform="rotate(-14 400 360)"
        />
        <ellipse
          cx="400"
          cy="360"
          rx="310"
          ry="210"
          transform="rotate(-14 400 360)"
        />
        <ellipse
          cx="400"
          cy="360"
          rx="410"
          ry="285"
          transform="rotate(-14 400 360)"
        />
        <ellipse
          cx="400"
          cy="360"
          rx="520"
          ry="370"
          transform="rotate(-14 400 360)"
        />
      </g>
    </svg>

    <div v-if="!embedded" class="absolute top-5 right-5 z-10 phone:top-3 phone:right-3">
      <ThemeControl compact />
    </div>

    <section
      class="relative z-10 flex w-full flex-col items-center"
      :class="maxWidth === 'md' ? 'max-w-[540px]' : 'max-w-[420px]'"
      :aria-label="label"
      :aria-busy="busy || undefined"
    >
      <div v-if="$slots.header" class="mb-5 flex w-full items-center justify-between gap-3 px-1">
        <slot name="header" />
      </div>
      <BrandWordmark v-else-if="!embedded" class="mb-7" />

      <div class="auth-stage-card w-full rounded-[20px] border border-line bg-surface p-8 phone:p-6">
        <slot />
      </div>

      <div v-if="$slots.footer" class="mt-5 w-full text-center text-xs text-muted">
        <slot name="footer" />
      </div>
    </section>
  </component>
</template>
