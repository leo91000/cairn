<script setup lang="ts">
import {
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
  watch,
} from 'vue'
import { LogOut, Settings } from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{ email: string, disabled?: boolean }>()
const emit = defineEmits<{ settings: [], signOut: [] }>()
const id = useId()
const root = ref<HTMLElement>()
const trigger = ref<HTMLButtonElement>()
const menu = ref<HTMLElement>()
const open = ref(false)
const position = ref({ left: '0px', top: '0px' })

function place() {
  if (!open.value || !trigger.value)
    return
  const anchor = trigger.value.getBoundingClientRect()
  position.value = {
    left: `${Math.max(12, Math.min(anchor.right - 232, innerWidth - 244))}px`,
    top: `${anchor.bottom + 6}px`,
  }
}

function close(restoreFocus = true) {
  menu.value?.hidePopover()
  open.value = false
  if (restoreFocus)
    trigger.value?.focus({ preventScroll: true })
}

async function show(last = false) {
  if (props.disabled)
    return
  open.value = true
  await nextTick()
  menu.value?.showPopover()
  place()
  const items = menu.value?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')
  items?.[last ? items.length - 1 : 0]?.focus({ preventScroll: true })
}

function navigate(event: KeyboardEvent) {
  if (!event.metaKey && !event.ctrlKey && !event.altKey)
    event.stopPropagation()
  if (event.key === 'Escape') {
    event.preventDefault()
    event.stopPropagation()
    close()
  }
  else if (event.key === 'Tab') {
    close(false)
  }
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const items = Array.from(menu.value?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]') || [])
    const current = items.indexOf(document.activeElement as HTMLButtonElement)
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (current + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length
    items[next]?.focus()
  }
}

// Focus follows the outside click; only Escape and menu actions return it to the trigger.
function outside(event: Event) {
  if (open.value && event.target instanceof Node && !root.value?.contains(event.target))
    close(false)
}

watch(() => props.disabled, disabled => disabled && close())
onMounted(() => {
  document.addEventListener('pointerdown', outside, true)
  document.addEventListener('focusin', outside)
  window.addEventListener('resize', place)
})
onBeforeUnmount(() => {
  menu.value?.hidePopover()
  document.removeEventListener('pointerdown', outside, true)
  document.removeEventListener('focusin', outside)
  window.removeEventListener('resize', place)
})
</script>

<template>
  <div ref="root" class="shrink-0">
    <button
      ref="trigger"
      type="button"
      aria-label="Account"
      :title="email"
      aria-haspopup="menu"
      :aria-expanded="open"
      :aria-controls="`${id}-menu`"
      :disabled="disabled"
      class="grid size-11 place-items-center rounded-full border border-line bg-soft text-sm font-semibold text-accent hover:bg-hover focus-visible:outline-2 focus-visible:outline-offset-3 focus-visible:outline-accent disabled:opacity-50"
      @click="open ? close() : show()"
      @keydown.down.prevent="show()"
      @keydown.up.prevent="show(true)"
    >
      {{ email.slice(0, 1).toUpperCase() }}
    </button>
    <div
      :id="`${id}-menu`"
      ref="menu"
      popover="manual"
      role="menu"
      aria-label="Account"
      :style="position"
      class="fixed inset-auto m-0 w-58 rounded-xl border border-line bg-raised p-1.5 text-ink shadow-lift"
      @keydown="navigate"
    >
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        class="flex min-h-11 w-full items-center gap-3 rounded-lg px-3 text-sm hover:bg-hover focus-visible:bg-soft focus-visible:outline-none"
        @click="close(); emit('settings')"
      >
        <Icon :name="Settings" :size="17" />Account settings
      </button>
      <div role="separator" class="my-1.5 border-t border-line" />
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        class="flex min-h-11 w-full items-center gap-3 rounded-lg px-3 text-sm text-danger hover:bg-danger-surface focus-visible:bg-danger-surface focus-visible:outline-none"
        @click="close(); emit('signOut')"
      >
        <Icon :name="LogOut" :size="17" />Sign out
      </button>
    </div>
  </div>
</template>
