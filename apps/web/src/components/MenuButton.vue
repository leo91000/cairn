<script setup lang="ts">
import type { ClassValue } from 'vue'
import {
  nextTick,
  onBeforeUnmount,
  ref,
  useId,
} from 'vue'

// A trigger and a small action menu in the top layer, so it is never clipped by a scroller or a
// rounded mosaic and still opens above modal dialogs. Arrow keys, Home/End, Escape and Tab follow
// the menu button pattern; focus returns to the trigger.
const props = withDefaults(defineProps<{
  label: string
  menuLabel?: string
  title?: string
  disabled?: boolean
  triggerClass?: ClassValue
  placement?: 'below' | 'above'
  /** Which trigger edge the menu lines up with. */
  align?: 'start' | 'end'
  width?: number
}>(), { placement: 'below', align: 'end', width: 224 })
const emit = defineEmits<{ open: [], close: [] }>()
const trigger = ref<HTMLButtonElement>()
const menu = ref<HTMLElement>()
const open = ref(false)
const position = ref<Record<string, string>>({})
const id = useId()
let frame = 0

function place() {
  const rect = trigger.value!.getBoundingClientRect()
  const height = menu.value?.offsetHeight ?? 0
  const left = Math.max(8, Math.min(props.align === 'start' ? rect.left : rect.right - props.width, innerWidth - props.width - 8))
  const below = rect.bottom + 6
  const above = rect.top - height - 6
  const top = props.placement === 'above'
    ? (above >= 8 ? above : below)
    : (below + height > innerHeight - 8 && above >= 8 ? above : below)
  position.value = { left: `${left}px`, top: `${Math.max(8, top)}px`, width: `${props.width}px` }
}

function items() {
  return Array.from(menu.value?.querySelectorAll<HTMLElement>('[role="menuitem"]:not(:disabled):not([aria-disabled="true"])') ?? [])
}

function dismiss() {
  hide(false)
}

async function show(focus: 'first' | 'last' = 'first') {
  if (open.value || props.disabled)
    return
  open.value = true
  emit('open')
  await nextTick()
  menu.value?.showPopover?.()
  place()
  const list = items()
  ;(focus === 'first' ? list[0] : list.at(-1))?.focus({ preventScroll: true })
  document.addEventListener('pointerdown', outside, true)
  window.addEventListener('resize', dismiss)
  window.addEventListener('scroll', scrolled, true)
}

function hide(restore = true) {
  if (!open.value)
    return
  open.value = false
  document.removeEventListener('pointerdown', outside, true)
  window.removeEventListener('resize', dismiss)
  window.removeEventListener('scroll', scrolled, true)
  cancelAnimationFrame(frame)
  frame = 0
  if (menu.value?.matches(':popover-open'))
    menu.value.hidePopover()
  if (restore)
    trigger.value?.focus({ preventScroll: true })
  emit('close')
}

// A streaming conversation scrolls on its own: the menu follows its trigger, and closes only once
// the trigger is gone or out of sight.
function scrolled(event: Event) {
  if (menu.value?.contains(event.target as Node) || frame)
    return
  frame = requestAnimationFrame(() => {
    frame = 0
    const rect = trigger.value?.isConnected ? trigger.value.getBoundingClientRect() : undefined
    if (!rect || rect.bottom < 0 || rect.top > innerHeight || !rect.width)
      hide(false)
    else if (open.value)
      place()
  })
}

function outside(event: Event) {
  const target = event.target as Node
  if (!menu.value?.contains(target) && !trigger.value?.contains(target))
    hide(false)
}

function toggle() {
  if (open.value)
    hide()
  else
    void show()
}

function triggerKeydown(event: KeyboardEvent) {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    void show(event.key === 'ArrowDown' ? 'first' : 'last')
  }
}

function menuKeydown(event: KeyboardEvent) {
  // Keep arrows and Escape from reaching a surrounding viewer or dialog.
  event.stopPropagation()
  if (event.key === 'Escape') {
    event.preventDefault()
    hide()
  }
  else if (event.key === 'Tab') {
    hide(false)
  }
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const list = items()
    const current = list.indexOf(document.activeElement as HTMLElement)
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? list.length - 1 : (current + (event.key === 'ArrowDown' ? 1 : -1) + list.length) % list.length
    list[next]?.focus()
  }
}

/** Closes the menu, then runs the chosen action (focus goes back to the trigger first). */
function select(action: () => void, restore = true) {
  hide(restore)
  action()
}

onBeforeUnmount(() => hide(false))
defineExpose({ show, hide })
</script>

<template>
  <button
    ref="trigger"
    type="button"
    :class="triggerClass"
    :aria-label="label"
    :title="title ?? label"
    aria-haspopup="menu"
    :aria-expanded="open"
    :aria-controls="open ? id : undefined"
    :disabled="disabled"
    @click.stop="toggle"
    @keydown="triggerKeydown"
  >
    <slot name="trigger" :open="open" />
  </button>
  <div
    :id="id"
    ref="menu"
    popover="manual"
    role="menu"
    :aria-label="menuLabel ?? label"
    :style="position"
    class="fixed inset-auto m-0 rounded-xl border border-line bg-raised p-1.5 text-ink shadow-[0_9px_25px_#0003]"
    @keydown="menuKeydown"
    @click.stop
  >
    <slot v-if="open" :select="select" :close="hide" />
  </div>
</template>

<style>
.menu-item {
  display: flex;
  width: 100%;
  min-height: 38px;
  align-items: center;
  gap: 10px;
  border-radius: 8px;
  padding: 7px 10px;
  text-align: left;
  font-size: 13px;
  color: var(--color-ink);
  text-decoration: none;
}
.menu-item:hover:not(:disabled),
.menu-item:focus {
  background: var(--color-hover);
  outline: none;
}
.menu-item:disabled { opacity: .5; cursor: default; }
@media (pointer: coarse) {
  .menu-item { min-height: 44px; }
}
</style>
