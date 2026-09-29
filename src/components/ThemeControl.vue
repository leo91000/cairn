<script setup lang="ts">
import type { ThemePreference } from '../theme'
import { twMerge } from 'tailwind-merge'
import {
  nextTick,
  onBeforeUnmount,
  ref,
  useId,
} from 'vue'
import {
  Check,
  Monitor,
  Moon,
  Sun,
} from '../icons'
import { setThemePreference, themePreference } from '../theme'
import { iconButton } from '../ui'
import Icon from './Icon.vue'

const props = defineProps<{ compact?: boolean }>()
const id = useId()
const trigger = ref<HTMLButtonElement>()
const popup = ref<HTMLElement>()
const choices = [
  {
    value: 'system' as const,
    label: 'System',
    description: 'Follow your device',
    icon: Monitor,
  },
  {
    value: 'light' as const,
    label: 'Light',
    description: 'Light surfaces',
    icon: Sun,
  },
  {
    value: 'dark' as const,
    label: 'Dark',
    description: 'Dark surfaces',
    icon: Moon,
  },
]

function position() {
  if (!trigger.value || !popup.value)
    return
  const rect = trigger.value.getBoundingClientRect()
  popup.value.style.left = `${Math.max(12, Math.min(rect.right - 250, innerWidth - 262))}px`
  popup.value.style.top = `${Math.max(12, Math.min(rect.bottom + 9, innerHeight - popup.value.offsetHeight - 12))}px`
}

async function toggle() {
  if (popup.value?.matches(':popover-open')) {
    popup.value.hidePopover()
    return
  }

  popup.value?.showPopover()
  await nextTick()
  position()
  popup.value?.querySelector<HTMLInputElement>('input:checked')?.focus()
}

function choose(value: ThemePreference) {
  setThemePreference(value)
  if (props.compact) {
    popup.value?.hidePopover()
    trigger.value?.focus()
  }
}

function dismiss() {
  if (popup.value?.matches(':popover-open'))
    popup.value.hidePopover()
}

window.addEventListener('resize', dismiss)
onBeforeUnmount(() => window.removeEventListener('resize', dismiss))
</script>

<template>
  <div class="theme-control" :class="{ 'theme-compact': compact }">
    <button
      v-if="compact"
      ref="trigger"
      :class="twMerge(iconButton, 'icon-button theme-trigger border border-line w-9.5 h-9.5 text-ink bg-transparent border-transparent phone:min-w-11 phone:min-h-11')"
      :aria-label="`Appearance: ${themePreference}`"
      :title="`Appearance: ${themePreference}`"
      :popovertarget="id"
      @click.prevent="toggle"
    >
      <Icon v-if="themePreference === 'dark'" :name="Moon" :size="18" /><Icon v-else-if="themePreference === 'light'" :name="Sun" :size="18" /><Icon v-else :name="Monitor" :size="18" />
    </button>
    <div
      :id="id"
      ref="popup"
      :popover="compact ? 'auto' : undefined"
      :class="{ 'theme-popover': compact }"
    >
      <fieldset class="theme-options border-0 grid grid-cols-3 gap-3 phone:gap-2 p-0 m-0">
        <legend :class="compact ? 'theme-menu-heading' : 'sr-only'">
          Appearance
        </legend>
        <label
          v-for="choice in choices"
          :key="choice.value"
          class="theme-choice relative flex flex-col gap-[7px] border border-line rounded-[10px] cursor-pointer bg-inset text-ink p-3.5 m-0 phone:p-2.5"
          :class="{ selected: themePreference === choice.value }"
        >
          <input
            type="radio"
            :name="`${id}-theme`"
            :value="choice.value"
            :checked="themePreference === choice.value"
            @change="choose(choice.value)"
          >
          <span v-if="!compact" class="theme-preview flex gap-1.5 h-16 rounded-[6px] bg-[#f2f1ff] border border-[#c2c0d6] mb-1.5 overflow-hidden border-[#d8d5f0] phone:h-13 p-[7px]" :data-preview="choice.value"><i /><span><i /><i /><i /></span></span>
          <span class="theme-choice-title flex items-center gap-2 text-xs text-ink phone:gap-[5px]"><Icon :name="choice.icon" :size="16" /><strong>{{ choice.label }}</strong><Icon v-if="themePreference === choice.value" :name="Check" :size="14" /></span>
          <small>{{ choice.description }}</small>
        </label>
      </fieldset>
    </div>
  </div>
</template>
