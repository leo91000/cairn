<script setup lang="ts">
import type { SetupTask } from '../plugin-setup'
import { twMerge } from 'tailwind-merge'
import { ref } from 'vue'
import { ArrowUpRight, Check, Copy } from '../icons'
import {
  buttonBase,
  buttonSizes,
  buttonVariants,
  iconButton,
} from '../ui'
import Icon from './Icon.vue'

// What the owner does on the service's site before Cairn can connect, ticked off as they
// go. Ticks only help the owner keep their place; nothing is saved.
const props = defineProps<{
  tasks: SetupTask[]
  callbackUrl?: string
}>()
const done = ref<boolean[]>(props.tasks.map(() => false))
const copied = ref(false)

async function copy() {
  try {
    await navigator.clipboard.writeText(props.callbackUrl ?? '')
    copied.value = true
  }
  catch {
    copied.value = false
  }
}
</script>

<template>
  <ol class="grid gap-2" aria-label="What to do first">
    <li
      v-for="(task, index) in tasks"
      :key="task.title"
      class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-x-3 gap-y-2 rounded-xl border border-line px-3.5 py-3 phone:grid-cols-[auto_minmax(0,1fr)]"
      :class="done[index] ? 'bg-inset' : 'bg-surface'"
    >
      <input
        v-model="done[index]"
        type="checkbox"
        class="mt-0.5 w-auto!"
        :aria-label="`${task.title} done`"
      >
      <div class="min-w-0">
        <p class="text-xs font-semibold" :class="done[index] ? 'text-muted line-through' : ''">
          {{ index + 1 }}. {{ task.title }}
        </p>
        <p v-if="task.text" class="mt-0.5 text-2xs leading-relaxed text-muted">
          {{ task.text }}
        </p>
        <div v-if="task.copy === 'callbackUrl'" class="copy-field mt-2 flex items-center rounded-lg border border-line pr-[5px]">
          <input
            readonly
            :value="callbackUrl"
            aria-label="Callback URL"
            class="font-mono text-2xs!"
          >
          <button
            type="button"
            :class="twMerge(iconButton, 'icon-button')"
            :aria-label="copied ? 'Callback URL copied' : 'Copy callback URL'"
            :disabled="!callbackUrl"
            @click="copy"
          >
            <Icon :name="copied ? Check : Copy" :size="15" />
          </button>
        </div>
      </div>
      <a
        v-if="task.url"
        class="phone:col-start-2 phone:justify-self-start"
        :class="[buttonBase, buttonVariants.default, buttonSizes.small]"
        :href="task.url"
        target="_blank"
        rel="noopener noreferrer"
        @click="done[index] = true"
      >{{ task.link || 'Open' }}<Icon :name="ArrowUpRight" :size="13" /></a>
    </li>
  </ol>
</template>
