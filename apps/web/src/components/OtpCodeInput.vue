<script setup lang="ts">
import { computed, ref, useId } from 'vue'

const props = withDefaults(defineProps<{
  modelValue: string
  label: string
  length?: number
  disabled?: boolean
  required?: boolean
}>(), {
  length: 8,
  disabled: false,
  required: true,
})

const emit = defineEmits<{ 'update:modelValue': [value: string] }>()
const inputId = useId()
const focused = ref(false)
const characters = computed(() => Array.from({ length: props.length }, (_, index) => props.modelValue[index] ?? ''))
const activeIndex = computed(() => Math.min(props.modelValue.length, props.length - 1))

function onInput(event: Event) {
  emit('update:modelValue', (event.target as HTMLInputElement).value.slice(0, props.length))
}
</script>

<template>
  <div class="grid gap-2">
    <label :for="inputId" class="text-xs font-semibold text-ink">
      {{ label }}
    </label>
    <div class="relative">
      <div
        class="grid gap-2 phone:gap-1.5"
        :style="{ gridTemplateColumns: `repeat(${length}, minmax(0, 1fr))` }"
        aria-hidden="true"
      >
        <div
          v-for="(char, index) in characters"
          :key="index"
          class="grid h-12 place-items-center rounded-[11px] border font-heading text-lg font-bold transition-[border-color,background-color,box-shadow] phone:h-11 phone:text-base"
          :class="[
            focused && index === activeIndex
              ? 'border-accent bg-surface shadow-(--shadow-focus)'
              : char
                ? 'border-line bg-surface'
                : 'border-control bg-inset',
            disabled ? 'opacity-50' : '',
          ]"
        >
          <span v-if="char">{{ char }}</span>
          <span
            v-else-if="focused && index === activeIndex"
            class="h-5 w-0.5 rounded-full bg-accent"
          />
        </div>
      </div>
      <!-- A single real input overlays the visual cells so autofill, paste, keyboard navigation and tests work natively. -->
      <input
        :id="inputId"
        :value="modelValue"
        type="text"
        inputmode="numeric"
        autocomplete="one-time-code"
        :maxlength="length"
        :required="required"
        :disabled="disabled"
        class="absolute inset-0 h-full w-full cursor-text border-0! bg-transparent! p-0! text-transparent! caret-transparent shadow-none! outline-none! selection:bg-transparent"
        @input="onInput"
        @focus="focused = true"
        @blur="focused = false"
      >
    </div>
  </div>
</template>
