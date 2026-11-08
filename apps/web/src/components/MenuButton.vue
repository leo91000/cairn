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
  /** Ids of content (such as the header slot) that screen readers announce with the menu. */
  describedby?: string
}>(), { placement: 'below', align: 'end', width: 224 })
const emit = defineEmits<{ open: [], close: [] }>()
const trigger = ref<HTMLButtonElement>()
const menu = ref<HTMLElement>()
const open = ref(false)
const position = ref<Record<string, string>>({})
const id = useId()
let frame = 0
let contents: MutationObserver | undefined

function place() {
  const rect = trigger.value!.getBoundingClientRect()
  // The natural height, not the height a previous placement capped.
  const box = menu.value
  const height = box ? box.scrollHeight + box.offsetHeight - box.clientHeight : 0
  const left = Math.max(8, Math.min(props.align === 'start' ? rect.left : rect.right - props.width, innerWidth - props.width - 8))
  const below = rect.bottom + 6
  const roomBelow = innerHeight - 8 - below
  const roomAbove = rect.top - 6 - 8
  // When neither side fits, the roomier one scrolls the menu instead of cutting off its last items.
  const fitsBelow = height <= roomBelow
  const fitsAbove = height <= roomAbove
  const useAbove = props.placement === 'above'
    ? fitsAbove || (!fitsBelow && roomAbove > roomBelow)
    : !fitsBelow && (fitsAbove || roomAbove > roomBelow)
  const top = useAbove ? Math.max(8, rect.top - 6 - height) : below
  position.value = {
    left: `${left}px`,
    top: `${top}px`,
    maxHeight: `${useAbove ? rect.top - 6 - top : roomBelow}px`,
  }
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
  // Focus once the height limit applies, so a scrolling menu shows the item it focuses.
  await nextTick()
  if (!open.value)
    return
  const list = items()
  const item = focus === 'first' ? list[0] : list.at(-1)
  item?.focus({ preventScroll: true })
  item?.scrollIntoView({ block: 'nearest' })
  // An item that is removed or disabled while focused would drop focus to the page, where the
  // menu's keys no longer reach it: focus moves to the next enabled item (the first one when the
  // focused item was removed) instead. Focus the app moved elsewhere on purpose stays there.
  contents = new MutationObserver(() => {
    const active = document.activeElement as HTMLElement | null
    const lost = !active || active === document.body
    const disabledHere = !!active && !!menu.value?.contains(active) && active.matches(':disabled, [aria-disabled="true"]')
    if (!open.value || (!lost && !disabledHere))
      return
    const all = Array.from(menu.value?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])
    const at = active ? all.indexOf(active) : -1
    const enabled = items()
    const next = at < 0 ? enabled[0] : [...all.slice(at + 1), ...all.slice(0, at)].find(item => enabled.includes(item))
    ;(next ?? trigger.value)?.focus({ preventScroll: true })
  })
  if (menu.value) {
    contents.observe(menu.value, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ['disabled', 'aria-disabled'],
    })
  }

  document.addEventListener('pointerdown', outside, true)
  window.addEventListener('resize', dismiss)
  window.addEventListener('scroll', scrolled, true)
}

function hide(restore = true) {
  if (!open.value)
    return
  open.value = false
  contents?.disconnect()
  contents = undefined
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
    // With no item focused (after a click on the menu's own content), arrows start from either end.
    const step = event.key === 'ArrowDown' ? 1 : -1
    const next = event.key === 'Home'
      ? 0
      : event.key === 'End' || (current < 0 && step < 0)
        ? list.length - 1
        : (current + step + list.length) % list.length
    list[next]?.focus()
  }
}

/** Closes the menu, then runs the chosen action (focus goes back to the trigger first). */
function select(action: () => void, restore = true) {
  hide(restore)
  action()
}

onBeforeUnmount(() => hide(false))
defineExpose({ show, hide, trigger })
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
  <!-- A header (what the menu is about) sits beside the menu, which may own only its items. -->
  <div
    ref="menu"
    popover="manual"
    tabindex="-1"
    :style="{ width: `${width}px`, ...position }"
    class="fixed inset-auto m-0 overflow-y-auto overscroll-contain rounded-xl border border-line bg-raised p-1.5 text-ink shadow-[0_9px_25px_#0003] outline-none"
    @keydown="menuKeydown"
    @click.stop
  >
    <template v-if="open">
      <slot name="header" />
      <div
        :id="id"
        role="menu"
        :aria-label="menuLabel ?? label"
        :aria-describedby="describedby"
      >
        <slot :select="select" :close="hide" />
      </div>
    </template>
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
