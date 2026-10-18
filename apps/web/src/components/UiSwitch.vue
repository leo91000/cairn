<script setup lang="ts">
// A native checkbox drawn as a switch. The label names what it turns on, including the
// resource (e.g. "Can run its conversations on gpu-box"); the wrapper keeps a 44px target.
defineProps<{
  label: string
  disabled?: boolean
  // Elevated rights such as exclusive reservations show in the power colour.
  power?: boolean
}>()
const model = defineModel<boolean>({ required: true })
</script>

<template>
  <label class="inline-grid min-h-8 min-w-11 shrink-0 cursor-pointer place-items-center phone:min-h-11" :class="disabled ? 'cursor-not-allowed' : ''">
    <input
      v-model="model"
      type="checkbox"
      role="switch"
      class="ui-switch"
      :class="{ power }"
      :aria-label="label"
      :disabled="disabled"
    >
  </label>
</template>

<style scoped>
.ui-switch {
  appearance: none;
  position: relative;
  width: 38px;
  height: 22px;
  padding: 0;
  border: 0;
  border-radius: 999px;
  background: var(--color-control);
  transition: background-color 0.18s;
  cursor: inherit;
}
.ui-switch::after {
  content: '';
  position: absolute;
  top: 3px;
  left: 3px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: #fff;
  box-shadow: 0 1px 2px #0003;
  transition: transform 0.2s var(--ease-signal);
}
.ui-switch:checked { background: var(--color-success); }
.ui-switch.power:checked { background: var(--color-power); }
.ui-switch:checked::after { transform: translateX(16px); }
.ui-switch:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 3px; box-shadow: none; }
.ui-switch:disabled { opacity: 0.45; }
@media (prefers-reduced-motion: reduce) {
  .ui-switch, .ui-switch::after { transition: none; }
}
</style>
