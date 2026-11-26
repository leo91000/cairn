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
const selection = ref({ start: 0, end: 0 })
const characters = computed(() => Array.from({ length: props.length }, (_, index) => props.modelValue[index] ?? ''))
const caretIndex = computed(() => Math.min(selection.value.start, props.length - 1))
const caretAtEnd = computed(() => selection.value.start === props.length)
const hasSelection = computed(() => selection.value.start !== selection.value.end)

function updateSelection(event: Event) {
  const input = event.target as HTMLInputElement
  const start = input.selectionStart ?? input.value.length
  const end = input.selectionEnd ?? start
  selection.value = { start, end }
}

function focus(event: FocusEvent) {
  focused.value = true
  updateSelection(event)
}

function onInput(event: Event) {
  const input = event.target as HTMLInputElement
  const selectionStart = input.selectionStart ?? input.value.length
  const selectionEnd = input.selectionEnd ?? selectionStart
  const onlyDigits = (value: string) => value.replace(/\D/g, '').slice(0, props.length)
  const digits = onlyDigits(input.value)
  if (digits !== input.value) {
    const start = onlyDigits(input.value.slice(0, selectionStart)).length
    const end = onlyDigits(input.value.slice(0, selectionEnd)).length
    input.value = digits
    input.setSelectionRange(start, end)
  }

  updateSelection(event)
  emit('update:modelValue', digits)
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
          class="relative grid h-12 place-items-center rounded-[11px] border font-heading text-lg font-bold transition-[border-color,background-color,box-shadow] phone:h-11 phone:text-base"
          :class="[
            focused && index >= selection.start && index < selection.end
              ? 'border-accent bg-soft'
              : focused && !hasSelection && index === caretIndex
                ? 'border-accent bg-surface shadow-(--shadow-focus)'
                : char
                  ? 'border-line bg-surface'
                  : 'border-control bg-inset',
            disabled ? 'opacity-50' : '',
          ]"
        >
          <span v-if="char">{{ char }}</span>
          <span
            v-if="focused && !hasSelection && index === caretIndex"
            class="absolute h-5 w-0.5 rounded-full bg-accent"
            :class="caretAtEnd ? 'right-1' : char ? 'left-1' : ''"
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
        :required="required"
        :disabled="disabled"
        class="absolute inset-0 h-full w-full cursor-text border-0! bg-transparent! p-0! text-transparent! caret-transparent shadow-none! outline-none! selection:bg-transparent"
        @input="onInput"
        @focus="focus"
        @select="updateSelection"
        @click="updateSelection"
        @keyup="updateSelection"
        @blur="focused = false"
      >
    </div>
  </div>
</template>
