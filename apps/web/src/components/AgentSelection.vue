<script setup lang="ts">
import type { SelectionItem } from '../agent-access'
import { computed, ref } from 'vue'
import { Search, X } from '../icons'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'

// "All, including future" or a chosen subset, with search and filters for long lists.
const props = defineProps<{
  title: string
  noun: string
  items: SelectionItem[]
}>()
const model = defineModel<string[] | null>({ required: true })
const query = ref('')
const selectedOnly = ref(false)
const unavailableOnly = ref(false)
const searchable = computed(() => props.items.length >= 8)
const mode = computed({
  get: () => model.value === null ? 'all' : 'choose',
  set: (value: 'all' | 'choose') => {
    if (value === (model.value === null ? 'all' : 'choose'))
      return
    model.value = value === 'all' ? null : []
  },
})
const selected = computed(() => new Set(model.value ?? []))
const filtered = computed(() => {
  const words = query.value.trim().toLowerCase()
  return props.items.filter(item => (!words || `${item.name} ${item.detail ?? ''}`.toLowerCase().includes(words))
    && (!selectedOnly.value || selected.value.has(item.id))
    && (!unavailableOnly.value || item.unavailable))
})
const narrowed = computed(() => Boolean(query.value.trim() || selectedOnly.value || unavailableOnly.value))
// Selected keys whose target was deleted stay visible until removed.
const missing = computed(() => (model.value ?? []).filter(id => !props.items.some(item => item.id === id)))

function toggle(id: string, on: boolean) {
  const current = model.value ?? []
  model.value = on ? [...current.filter(value => value !== id), id] : current.filter(value => value !== id)
}

function selectAll() {
  model.value = [...new Set([...(model.value ?? []), ...filtered.value.map(item => item.id)])]
}

function clear() {
  const visible = new Set(filtered.value.map(item => item.id))
  model.value = narrowed.value ? (model.value ?? []).filter(id => !visible.has(id)) : []
}
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div class="flex w-fit max-w-full gap-0.5 rounded-lg border border-line p-0.5" role="group" :aria-label="`${title} access`">
      <button
        v-for="option in [{ value: 'all', label: 'All, including future' }, { value: 'choose', label: 'Choose' }] as const"
        :key="option.value"
        type="button"
        class="min-h-9 rounded-md px-3 text-xs font-semibold transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent phone:min-h-11"
        :class="mode === option.value ? 'bg-hover text-ink' : 'text-muted hover:text-ink'"
        :aria-pressed="mode === option.value"
        @click="mode = option.value"
      >
        {{ option.label }}
      </button>
    </div>
    <div v-if="mode === 'all'" class="text-sm leading-relaxed text-muted">
      <slot name="all" />
    </div>
    <template v-else>
      <div class="flex min-w-0 flex-wrap items-center gap-2">
        <label v-if="searchable" class="search-field flex min-w-0 flex-1 basis-56 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle"><Icon :name="Search" :size="16" /><input
          v-model="query"
          type="search"
          :placeholder="`Search ${noun}s`"
          :aria-label="`Search ${noun}s`"
        ></label>
        <template v-if="searchable">
          <button
            type="button"
            class="min-h-8 rounded-full border px-3 text-2xs font-semibold phone:min-h-11"
            :class="selectedOnly ? 'border-accent bg-soft text-accent' : 'border-line text-muted hover:bg-hover'"
            :aria-pressed="selectedOnly"
            @click="selectedOnly = !selectedOnly"
          >
            Selected
          </button>
          <button
            v-if="items.some(item => item.unavailable)"
            type="button"
            class="min-h-8 rounded-full border px-3 text-2xs font-semibold phone:min-h-11"
            :class="unavailableOnly ? 'border-accent bg-soft text-accent' : 'border-line text-muted hover:bg-hover'"
            :aria-pressed="unavailableOnly"
            @click="unavailableOnly = !unavailableOnly"
          >
            Unavailable
          </button>
        </template>
        <span class="text-xs text-muted tabular-nums" aria-live="polite">{{ selected.size }} of {{ items.length }}</span>
        <span class="ml-auto flex gap-1.5">
          <UiButton size="small" :disabled="!filtered.length" @click="selectAll">{{ narrowed ? 'Select all (filtered)' : 'Select all' }}</UiButton>
          <UiButton size="small" :disabled="!selected.size" @click="clear">Clear</UiButton>
        </span>
      </div>
      <ul v-if="filtered.length || missing.length" class="min-w-0 divide-y divide-line overflow-hidden rounded-xl border border-line bg-surface">
        <li v-for="item in filtered" :key="item.id" class="min-w-0">
          <label class="flex min-h-11 cursor-pointer flex-row! items-center gap-3! px-4 py-2.5 text-sm font-normal! text-ink! hover:bg-hover/40">
            <input
              type="checkbox"
              class="size-4 w-4! shrink-0 accent-[var(--color-accent)]"
              :aria-label="item.label ?? item.name"
              :checked="selected.has(item.id)"
              @change="toggle(item.id, ($event.target as HTMLInputElement).checked)"
            >
            <span class="min-w-0 flex-1">
              <span class="block truncate">{{ item.name }}</span>
              <span v-if="item.detail" class="block truncate text-2xs text-muted">{{ item.detail }}</span>
            </span>
            <AccessChip v-if="item.unavailable" tone="unavailable">{{ item.unavailable }}</AccessChip>
          </label>
          <slot name="item" :item="item" :selected="selected.has(item.id)" />
        </li>
        <li v-for="id in missing" :key="id" class="flex min-h-11 items-center gap-3 px-4 py-2.5 text-sm">
          <span class="min-w-0 flex-1 truncate text-muted"><slot :id="id" name="missing">{{ id }}</slot></span>
          <AccessChip tone="missing">
            Not found
          </AccessChip>
          <UiButton size="small" :aria-label="`Remove missing ${noun}`" @click="toggle(id, false)">
            <Icon :name="X" :size="13" />Remove
          </UiButton>
        </li>
      </ul>
      <p v-else-if="items.length" class="text-sm text-muted">
        No {{ noun }} matches these filters.
      </p>
      <div v-else class="text-sm text-muted">
        <slot name="empty">
          Nothing to choose yet.
        </slot>
      </div>
    </template>
  </div>
</template>
