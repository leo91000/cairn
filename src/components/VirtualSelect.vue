<script setup lang="ts">
import type { CSSProperties } from 'vue'
import type { IconName } from '../icons'
import type { SelectOption } from '../select'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
  watch,
} from 'vue'
import {
  Check,
  ChevronDown,
  LoaderCircle,
  Search,
  SearchX,
  X,
} from '../icons'
import { filterOptions, selectRows, visibleRows } from '../select'
import Icon from './Icon.vue'

const props = withDefaults(defineProps<{
  label: string
  options: SelectOption[]
  placeholder?: string
  searchPlaceholder?: string
  emptyText?: string
  icon?: IconName
  required?: boolean
  disabled?: boolean
  loading?: boolean
  clearable?: boolean
  compact?: boolean
  variant?: 'default' | 'ghost'
  hideLabel?: boolean
}>(), { placeholder: 'Select an option', emptyText: 'No options available' })
const model = defineModel<string>({ default: '' })
const id = useId()
const root = ref<HTMLElement>()
const input = ref<HTMLInputElement>()
const popup = ref<HTMLElement>()
const viewport = ref<HTMLElement>()
const open = ref(false)
const query = ref('')
const active = ref(-1)
const scrollTop = ref(0)
const viewportHeight = ref(280)
const position = ref<CSSProperties>({ visibility: 'hidden' })
const invalid = ref(false)
const selected = computed(() => props.options.find(option => option.value === model.value))
const filtered = computed(() => filterOptions(props.options, query.value))
const layout = computed(() => selectRows(filtered.value))
const rows = computed(() => visibleRows(layout.value.rows, scrollTop.value, viewportHeight.value))
const activeId = computed(() => rows.value.some(row => row.optionIndex === active.value) ? `${id}-option-${active.value}` : undefined)
const value = computed(() => open.value ? query.value : selected.value?.label ?? '')
const leadingIcon = computed(() => open.value ? Search : selected.value?.icon || props.icon)
let resize: ResizeObserver | undefined

function positionPopup() {
  if (!open.value || !input.value)
    return
  const anchor = input.value.parentElement!.getBoundingClientRect()
  const visual = window.visualViewport
  const left = visual?.offsetLeft ?? 0
  const top = visual?.offsetTop ?? 0
  const width = visual?.width ?? innerWidth
  const height = visual?.height ?? innerHeight
  const popupWidth = Math.min(Math.max(anchor.width, 290), width - 24)
  const below = top + height - anchor.bottom - 14
  const above = anchor.top - top - 14
  const desired = Math.min(layout.value.height || 96, 280) + 78
  const upwards = below < Math.min(desired, 210) && above > below
  const available = Math.max(90, upwards ? above : below)
  viewportHeight.value = Math.max(44, Math.min(layout.value.height || 96, 280, available - 78))
  const popupHeight = viewportHeight.value + 78
  position.value = {
    left: `${Math.max(left + 12, Math.min(anchor.left, left + width - popupWidth - 12))}px`,
    top: `${Math.max(top + 8, upwards ? anchor.top - popupHeight - 6 : anchor.bottom + 6)}px`,
    width: `${popupWidth}px`,
  }
}

function revealActive() {
  const row = layout.value.rows.find(row => row.optionIndex === active.value)
  if (!row || !viewport.value)
    return
  let next = viewport.value.scrollTop
  if (row.top < next)
    next = row.top
  else if (row.top + row.height > next + viewportHeight.value)
    next = row.top + row.height - viewportHeight.value
  viewport.value.scrollTop = next
  scrollTop.value = next
}

function firstEnabled(from = 0, direction = 1) {
  for (let index = from; index >= 0 && index < layout.value.options.length; index += direction) {
    if (!layout.value.options[index].disabled)
      return index
  }

  return -1
}

async function show(initialQuery = '') {
  if (props.disabled || open.value)
    return
  query.value = initialQuery
  open.value = true
  await nextTick()
  if (!open.value || !popup.value?.isConnected)
    return
  popup.value?.showPopover()
  positionPopup()
  const index = layout.value.options.findIndex(option => option.value === model.value && !option.disabled)
  active.value = index >= 0 ? index : firstEnabled()
  await nextTick()
  revealActive()
}

function close() {
  popup.value?.hidePopover()
  open.value = false
  query.value = ''
}

function choose(option: SelectOption) {
  if (option.disabled || props.loading)
    return
  model.value = option.value
  invalid.value = false
  close()
  input.value?.focus({ preventScroll: true })
}

function clear() {
  model.value = ''
  query.value = ''
  input.value?.focus({ preventScroll: true })
}

function updateQuery(event: Event) {
  const value = (event.target as HTMLInputElement).value
  if (!open.value)
    void show(value)
  else
    query.value = value
}

function keydown(event: KeyboardEvent) {
  if (event.isComposing)
    return
  if (event.key === 'Tab') {
    close()
    return
  }

  if (event.key === 'Escape' && open.value) {
    event.preventDefault()
    event.stopPropagation()
    close()
    return
  }

  if (event.key === 'Enter') {
    event.preventDefault()
    if (!open.value)
      void show()
    else if (active.value >= 0)
      choose(layout.value.options[active.value])
    return
  }

  if (['ArrowDown', 'ArrowUp', 'Home', 'End', 'PageDown', 'PageUp'].includes(event.key)) {
    if (!open.value && !event.key.startsWith('Arrow'))
      return
    event.preventDefault()
    if (!open.value) {
      void show()
      return
    }

    const direction = ['ArrowUp', 'End', 'PageUp'].includes(event.key) ? -1 : 1
    const step = event.key.startsWith('Page') ? Math.max(1, Math.floor(viewportHeight.value / 50)) : 1
    const start = event.key === 'Home' ? 0 : event.key === 'End' ? layout.value.options.length - 1 : Math.max(0, Math.min(layout.value.options.length - 1, active.value + direction * step))
    const index = firstEnabled(start, direction)
    if (index >= 0)
      active.value = index
    revealActive()
  }
  else if (!open.value && event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
    event.preventDefault()
    void show(event.key)
  }
}

function outside(event: Event) {
  if (open.value && event.target instanceof Node && !root.value?.contains(event.target))
    close()
}

function ancestorScroll(event: Event) {
  if (event.target !== viewport.value)
    positionPopup()
}

watch(query, async () => {
  active.value = firstEnabled()
  scrollTop.value = 0
  if (viewport.value)
    viewport.value.scrollTop = 0
  await nextTick()
  positionPopup()
})
watch(() => props.options, async () => {
  if (!open.value)
    return
  active.value = firstEnabled()
  await nextTick()
  positionPopup()
  revealActive()
})
watch(() => props.disabled, disabled => disabled && close())
watch(model, () => invalid.value = false)
onMounted(() => {
  document.addEventListener('pointerdown', outside, true)
  document.addEventListener('focusin', outside)
  document.addEventListener('scroll', ancestorScroll, true)
  window.addEventListener('resize', positionPopup)
  window.visualViewport?.addEventListener('resize', positionPopup)
  window.visualViewport?.addEventListener('scroll', positionPopup)
  resize = new ResizeObserver(positionPopup)
  if (root.value)
    resize.observe(root.value)
})
onBeforeUnmount(() => {
  close()
  resize?.disconnect()
  document.removeEventListener('pointerdown', outside, true)
  document.removeEventListener('focusin', outside)
  document.removeEventListener('scroll', ancestorScroll, true)
  window.removeEventListener('resize', positionPopup)
  window.visualViewport?.removeEventListener('resize', positionPopup)
  window.visualViewport?.removeEventListener('scroll', positionPopup)
})
</script>

<template>
  <div ref="root" class="virtual-select min-w-0 w-full relative" :class="{ 'vs-ghost': variant === 'ghost', 'vs-compact': compact, 'vs-open': open, 'vs-disabled': disabled, 'vs-invalid': invalid }">
    <label v-if="!hideLabel" :for="`${id}-input`" class="vs-label block mb-2 text-xs font-medium text-muted">{{ label }}</label>
    <div class="vs-control flex items-center gap-2.5 min-h-12 w-full pr-2.5 pl-[13px] bg-raised border border-control rounded-[10px] [transition:border-color_.15s,_box-shadow_.15s] phone:min-h-12 phone:gap-2 [@media(prefers-reduced-motion:_reduce)]:[transition:none] py-0">
      <span v-if="leadingIcon" class="vs-leading grid place-items-center w-[29px] h-[29px] rounded-lg text-muted bg-surface shrink-0"><Icon :name="leadingIcon" :size="17" /></span>
      <input
        :id="`${id}-input`"
        ref="input"
        :value="value"
        role="combobox"
        :aria-label="label"
        :aria-expanded="open"
        :aria-controls="`${id}-list`"
        :aria-activedescendant="open && !loading ? activeId : undefined"
        aria-autocomplete="list"
        :aria-required="required"
        :aria-invalid="invalid || undefined"
        :aria-describedby="invalid ? `${id}-error` : undefined"
        :disabled="disabled"
        :placeholder="open ? searchPlaceholder || `Search ${label.toLowerCase()}…` : placeholder"
        autocomplete="off"
        autocapitalize="off"
        :spellcheck="false"
        @click="show()"
        @focus="($event.target as HTMLInputElement).select()"
        @input="updateQuery"
        @keydown="keydown"
        @compositionstart="show()"
      >
      <button
        v-if="clearable && model && !disabled"
        type="button"
        class="vs-clear grid place-items-center border-0 bg-transparent text-muted min-w-[27px] min-h-8 cursor-pointer rounded-[6px] shrink-0 phone:min-w-7.5 phone:min-h-11 p-0"
        :aria-label="`Clear ${label.toLowerCase()}`"
        @click="clear"
      >
        <Icon :name="X" :size="14" />
      </button>
      <button
        type="button"
        class="vs-chevron grid place-items-center border-0 bg-transparent text-muted min-w-[27px] min-h-8 cursor-pointer rounded-[6px] shrink-0 phone:min-w-7.5 phone:min-h-11 p-0"
        tabindex="-1"
        :disabled="disabled"
        :aria-label="`Toggle ${label.toLowerCase()} options`"
        @mousedown.prevent
        @click="input?.focus({ preventScroll: true }); open ? close() : show()"
      >
        <Icon
          v-if="loading"
          :name="LoaderCircle"
          class="vs-spin animate-spin [@media(prefers-reduced-motion:_reduce)]:[animation:none]"
          :size="16"
        /><Icon v-else :name="ChevronDown" :size="16" />
      </button>
    </div>
    <input
      v-if="required"
      class="vs-validation absolute w-[1px] h-[1px] opacity-0 pointer-events-none border-0 bottom-0 left-0 p-0"
      tabindex="-1"
      aria-hidden="true"
      :value="selected && model ? model : ''"
      :disabled="disabled"
      required
      @invalid.prevent="invalid = true; input?.focus(); show()"
    >
    <span v-if="invalid" :id="`${id}-error`" class="vs-error block text-danger text-2xs mt-[7px]">Choose {{ label.toLowerCase() }} to continue.</span>
    <div
      ref="popup"
      popover="manual"
      class="vs-popup fixed inset-auto border border-line rounded-card bg-raised text-ink [box-shadow:0_16px_45px_light-dark(#26243c20,_#00000020),_0_3px_10px_light-dark(#26243c10,_#00000010)] overflow-hidden font-sans p-0 m-0"
      :style="position"
      @keydown.esc.prevent.stop="close(); input?.focus({ preventScroll: true })"
    >
      <header class="vs-popup-header h-9.5 flex items-center justify-between gap-3 text-3xs font-semibold text-muted bg-raised [border-bottom:1px_solid_light-dark(#e7e6f0,_var(--dark-border))] px-[13px] py-0">
        <span>{{ query ? 'Search results' : label }}</span><span class="vs-count text-micro font-normal rounded-[5px] bg-surface text-muted px-1.5 py-[3px]">{{ filtered.length.toLocaleString() }} {{ filtered.length === 1 ? 'option' : 'options' }}</span>
      </header>
      <div
        :id="`${id}-list`"
        ref="viewport"
        class="vs-viewport overflow-auto overscroll-contain [scrollbar-width:thin] [scrollbar-color:light-dark(#bbb9d5,_var(--dark-control-border))_transparent]"
        role="listbox"
        :aria-label="label"
        :aria-busy="loading"
        :style="{ height: `${viewportHeight}px` }"
        @scroll="scrollTop = ($event.target as HTMLElement).scrollTop"
      >
        <div v-if="loading" class="vs-empty flex flex-col gap-[7px] items-center justify-center h-full text-muted text-center p-2.5">
          <Icon :name="LoaderCircle" class="vs-spin animate-spin [@media(prefers-reduced-motion:_reduce)]:[animation:none]" :size="22" /><strong>Loading options…</strong>
        </div>
        <div v-else-if="!filtered.length" class="vs-empty flex flex-col gap-[7px] items-center justify-center h-full text-muted text-center p-2.5">
          <Icon :name="SearchX" :size="23" /><strong>{{ query ? 'No matches found' : emptyText }}</strong><span v-if="query">Try another name or keyword.</span>
        </div>
        <div v-else class="vs-spacer relative w-full" :style="{ height: `${layout.height}px` }">
          <template v-for="row in rows" :key="row.key">
            <div
              v-if="row.group"
              class="vs-group absolute inset-x-0 pt-2.5 pb-1 text-micro font-semibold tracking-[.09em] uppercase text-muted px-4.5"
              role="presentation"
              :style="{ top: `${row.top}px`, height: `${row.height}px` }"
            >
              {{ row.group }}
            </div>
            <div
              v-else-if="row.option"
              :id="`${id}-option-${row.optionIndex}`"
              role="option"
              class="vs-option absolute inset-x-[5px] flex items-center gap-2.5 rounded-[7px] cursor-pointer border-3 border-transparent bg-clip-padding px-2.5 py-[7px]"
              :aria-setsize="layout.options.length"
              :aria-posinset="row.optionIndex! + 1"
              :aria-selected="row.option.value === model"
              :aria-disabled="row.option.disabled || undefined"
              :aria-label="row.option.label"
              :aria-description="[row.option.group, row.option.description].filter(Boolean).join(' · ')"
              :class="{ 'is-active': active === row.optionIndex, 'is-selected': row.option.value === model, 'is-disabled': row.option.disabled }"
              :style="{ top: `${row.top}px`, height: `${row.height}px` }"
              @pointermove="!row.option.disabled && (active = row.optionIndex!)"
              @mousedown.prevent
              @click="choose(row.option)"
            >
              <span v-if="row.option.icon || icon" class="vs-option-icon grid place-items-center w-7.5 h-7.5 shrink-0 rounded-[9px] text-muted bg-surface border border-line"><Icon :name="row.option.icon || icon!" :size="17" /></span>
              <span class="vs-option-copy min-w-0 flex-1"><strong>{{ row.option.label }}</strong><span v-if="row.option.description">{{ row.option.description }}</span></span>
              <Icon
                v-if="row.option.value === model"
                :name="Check"
                class="vs-check shrink-0 text-accent"
                :size="16"
              /><span v-else-if="row.option.disabled" class="vs-unavailable text-micro">Unavailable</span>
            </div>
          </template>
        </div>
      </div>
      <footer class="vs-popup-footer h-10 flex items-center justify-between gap-2 text-subtle [border-top:1px_solid_light-dark(#e7e6f0,_var(--dark-border))] bg-inset text-micro px-[13px] py-0">
        <span><kbd>↑</kbd><kbd>↓</kbd> Navigate <kbd>↵</kbd> Choose</span><button
          v-if="query"
          type="button"
          @mousedown.prevent
          @click="query = ''; input?.focus({ preventScroll: true })"
        >
          Clear search
        </button><button
          v-else
          type="button"
          aria-label="Close options"
          @mousedown.prevent
          @click="close(); input?.focus({ preventScroll: true })"
        >
          <kbd>esc</kbd> Close
        </button>
      </footer>
      <span class="sr-only" role="status">{{ loading ? 'Loading options' : `${filtered.length} options available` }}</span>
    </div>
  </div>
</template>
