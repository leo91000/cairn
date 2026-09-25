<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { notify } from '../api'
import { highlight } from '../highlight'
import { Check, Copy } from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{
  code: string
  label: string
  language?: string
  copyCode?: string
}>()
const expanded = ref(false)
const copied = ref(false)
let timer: ReturnType<typeof setTimeout>
const shown = computed(() => expanded.value ? props.code : props.code.slice(0, 12000))
const html = computed(() => highlight(shown.value, props.language))

async function copy() {
  try {
    await navigator.clipboard.writeText(props.copyCode ?? props.code)
    copied.value = true
    clearTimeout(timer)
    timer = setTimeout(() => copied.value = false, 2000)
  }
  catch {
    notify('Clipboard unavailable. Select the text to copy it.')
  }
}

onBeforeUnmount(() => clearTimeout(timer))
</script>

<template>
  <div class="activity-code border border-line rounded-[9px] overflow-hidden bg-inset min-w-0 mx-0 my-2.5">
    <header>
      <span>{{ label }}</span><button class="code-copy flex items-center gap-1.5 border-0 bg-transparent text-muted text-3xs cursor-pointer min-h-8 shrink-0 phone:min-h-9.5" :aria-label="`Copy ${label}`" @click="copy">
        <Icon v-if="copied" :name="Check" :size="14" /><Icon v-else :name="Copy" :size="14" />{{ copied ? 'Copied' : 'Copy' }}
      </button>
    </header>
    <pre tabindex="0" :aria-label="label"><code v-html="html" /></pre>
    <button v-if="code.length > 12000" class="code-expand w-full border-0 [border-top:1px_solid_light-dark(#d9d8e7,_var(--dark-border))] text-ink bg-inset cursor-pointer text-2xs p-[11px]" @click="expanded = !expanded">
      {{ expanded ? 'Show less' : `Show all ${code.length.toLocaleString()} characters` }}
    </button>
  </div>
</template>
