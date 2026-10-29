<script setup lang="ts">
import type { PluginProject } from '../../../../packages/contracts/contracts'
import type { PluginSourceChoice } from '../plugin-sources'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { listSourceProjects } from '../../../../packages/contracts/generated/listSourceProjects'
import { api } from '../api'
import {
  Check,
  ExternalLink,
  GitBranch,
  Pencil,
  RefreshCw,
  Search,
  SearchX,
  X,
} from '../icons'
import { projectMatches, updatedLabel } from '../plugin-sources'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import PluginMark from './PluginMark.vue'
import UiButton from './UiButton.vue'

// The projects a plugin's project source offers through one of its instances.
const props = defineProps<{
  source: PluginSourceChoice
  instance: string
  disabled?: boolean
  selected: string
}>()
const emit = defineEmits<{ select: [project: PluginProject] }>()
const projects = ref<PluginProject[]>([])
const query = ref('')
const loading = ref(false)
const error = ref('')
const nextPage = ref<number | null>(1)
const browsing = ref(true)
const active = ref(-1)
const search = ref<HTMLInputElement>()
const list = ref<HTMLElement>()
const sentinel = ref<HTMLElement>()
const filtered = computed(() => projects.value.filter(project => projectMatches(project, query.value)))
const chosen = computed(() => projects.value.find(project => project.id === props.selected))
const needle = computed(() => query.value.trim().toLowerCase())
// Searching keeps fetching pages until enough matches are visible.
const needsMore = computed(() => nextPage.value !== null && !error.value && !!needle.value && filtered.value.length < 8)
// Ids are the package's own: the list uses positions.
const optionId = (index: number) => `plugin-project-${index}`

// A reload, or another source or instance, makes pages still loading irrelevant.
let generation = 0

async function load() {
  if (loading.value || nextPage.value === null)
    return
  const asked = generation
  loading.value = true
  error.value = ''
  try {
    const page = await listSourceProjects(api, {
      pluginId: props.source.pluginId,
      source: props.source.source,
      instance: props.instance,
      page: nextPage.value,
      query: '',
    })
    if (asked !== generation)
      return
    const known = new Set(projects.value.map(project => project.id))
    projects.value.push(...page.items.filter(project => !known.has(project.id)))
    nextPage.value = page.nextPage
  }
  catch (e) {
    if (asked === generation)
      error.value = (e as Error).message
  }
  finally {
    if (asked === generation)
      loading.value = false
  }

  if (needsMore.value)
    await load()
}

function select(project: PluginProject) {
  if (props.disabled || project.imported)
    return
  emit('select', project)
  browsing.value = false
}

async function change() {
  browsing.value = true
  await nextTick()
  search.value?.focus()
}

function move(step: number) {
  const options = filtered.value
  if (!options.length)
    return
  let index = active.value
  for (let tries = 0; tries < options.length; tries++) {
    index = (index + step + options.length) % options.length
    if (!options[index]!.imported)
      break
  }

  active.value = index
  nextTick(() => list.value?.querySelector(`#${optionId(index)}`)?.scrollIntoView({ block: 'nearest' }))
}

function keydown(event: KeyboardEvent) {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    move(event.key === 'ArrowDown' ? 1 : -1)
  }
  else if (event.key === 'Enter') {
    event.preventDefault()
    const selectable = filtered.value.filter(project => !project.imported)
    const project = filtered.value[active.value] ?? (selectable.length === 1 ? selectable[0] : undefined)
    if (project)
      select(project)
  }
  else if (event.key === 'Escape' && query.value) {
    event.stopPropagation()
    query.value = ''
  }
}

function reload() {
  generation++
  loading.value = false
  error.value = ''
  projects.value = []
  nextPage.value = 1
  active.value = -1
  void load()
}

function parts(value: string) {
  const at = needle.value ? value.toLowerCase().indexOf(needle.value) : -1
  return at < 0 ? [{ text: value, match: false }] : [{ text: value.slice(0, at), match: false }, { text: value.slice(at, at + needle.value.length), match: true }, { text: value.slice(at + needle.value.length), match: false }]
}

function hue(value: string) {
  let hash = 0
  for (const char of value)
    hash = (Math.imul(hash, 31) + char.charCodeAt(0)) | 0
  return (hash >>> 0) % 360
}

watch(needle, () => {
  active.value = -1
  list.value?.scrollTo({ top: 0 })
})
watch(() => `${props.source.key}:${props.instance}`, () => {
  browsing.value = true
  reload()
})
watch(needsMore, more => more && load())
let observer: IntersectionObserver | undefined
watch(sentinel, (element) => {
  observer?.disconnect()
  if (element && typeof IntersectionObserver !== 'undefined') {
    observer = new IntersectionObserver(entries => entries.some(entry => entry.isIntersecting) && !error.value && load(), { root: list.value, rootMargin: '120px' })
    observer.observe(element)
  }
}, { flush: 'post' })
onMounted(load)
onBeforeUnmount(() => observer?.disconnect())
</script>

<template>
  <section class="grid min-w-0 gap-3" :aria-label="`${source.title} projects`" :aria-busy="loading">
    <div v-if="chosen && !browsing" class="flex min-w-0 items-center gap-3 rounded-xl border border-accent bg-soft px-3.5 py-3 shadow-arcade">
      <span class="grid size-10 shrink-0 place-items-center rounded-lg text-sm font-semibold text-white" :style="{ background: `oklch(0.58 0.14 ${hue(chosen.id)})` }" aria-hidden="true">{{ chosen.name.slice(0, 1).toUpperCase() }}</span>
      <div class="grid min-w-0 flex-1 gap-0.5">
        <span class="flex min-w-0 items-center gap-1.5 text-sm font-semibold text-ink"><Icon :name="Check" :size="15" class="text-accent" /><span class="min-w-0 break-all">{{ chosen.name }}</span></span>
        <span class="flex min-w-0 items-center gap-1.5 text-xs text-muted"><Icon :name="GitBranch" :size="13" />{{ chosen.defaultBranch || 'default branch' }} · cloned when you save</span>
      </div>
      <UiButton
        type="button"
        size="small"
        :disabled="disabled"
        @click="change"
      >
        <Icon :name="Pencil" :size="14" />Change
      </UiButton>
    </div>
    <template v-else>
      <div class="flex min-w-0 items-center justify-between gap-3">
        <p class="flex min-w-0 items-center gap-2 text-xs text-muted">
          <PluginMark :plugin="source.pluginId" :brand="source.brand" :size="20" />
          <span>{{ source.description || `Pick a project from ${source.pluginName}.` }} It is cloned when you save.</span>
        </p>
        <button
          type="button"
          :class="iconButton"
          :disabled="disabled || loading"
          aria-label="Reload projects"
          title="Reload projects"
          @click="reload"
        >
          <Icon :name="RefreshCw" :size="15" :class="loading ? 'animate-spin' : ''" />
        </button>
      </div>
      <label class="search-field flex min-w-0 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle">
        <Icon :name="Search" :size="16" />
        <input
          ref="search"
          v-model="query"
          type="text"
          enterkeyhint="search"
          autocomplete="off"
          spellcheck="false"
          role="combobox"
          aria-label="Search projects"
          aria-autocomplete="list"
          aria-expanded="true"
          aria-controls="plugin-projects"
          :aria-activedescendant="active >= 0 ? optionId(active) : undefined"
          placeholder="Search by name, description, badge…"
          :disabled="disabled"
          @keydown="keydown"
        >
        <button
          v-if="query"
          type="button"
          class="grid size-6 place-items-center rounded text-muted hover:bg-soft hover:text-ink"
          aria-label="Clear search"
          @click="query = ''; search?.focus()"
        >
          <Icon :name="X" :size="14" />
        </button>
      </label>
      <div v-if="error" role="alert" class="flex min-w-0 flex-wrap items-center gap-3 rounded-lg border border-line bg-danger-surface px-4 py-3 text-sm text-danger">
        <span class="min-w-0 flex-1">{{ error }} <RouterLink :to="`/plugins/${source.pluginId}`" class="underline">{{ source.pluginName }}</RouterLink></span>
        <UiButton
          type="button"
          size="small"
          class="bg-surface"
          :disabled="disabled || loading"
          @click="load"
        >
          <Icon :name="RefreshCw" :size="14" />Retry
        </UiButton>
      </div>
      <div
        id="plugin-projects"
        ref="list"
        class="grid max-h-80 min-w-0 content-start overflow-y-auto overscroll-contain rounded-xl border border-line bg-inset p-1.5"
        role="listbox"
        aria-label="Available projects"
      >
        <template v-if="!projects.length && loading">
          <div
            v-for="n in 4"
            :key="n"
            class="flex animate-pulse items-center gap-3 px-2.5 py-2.5"
            aria-hidden="true"
          >
            <span class="size-9 shrink-0 rounded-lg bg-line" />
            <span class="grid flex-1 gap-2"><span class="h-3 w-2/5 rounded bg-line" /><span class="h-2.5 w-3/4 rounded bg-line/70" /></span>
          </div>
        </template>
        <div
          v-for="(project, index) in filtered"
          :id="optionId(index)"
          :key="project.id"
          role="option"
          :aria-selected="selected === project.id"
          :aria-disabled="disabled || project.imported"
          class="group flex min-w-0 cursor-pointer items-start gap-3 rounded-lg border px-2.5 py-2.5 transition-colors aria-disabled:cursor-not-allowed aria-disabled:opacity-55"
          :class="selected === project.id ? 'border-accent bg-soft' : active === index ? 'border-line bg-surface shadow-arcade' : 'border-transparent'"
          @mouseenter="active = index"
          @click="select(project)"
        >
          <span class="grid size-9 shrink-0 place-items-center rounded-lg text-sm font-semibold text-white" :style="{ background: `oklch(0.58 0.14 ${hue(project.id)})` }" aria-hidden="true">{{ project.name.slice(0, 1).toUpperCase() }}</span>
          <span class="grid min-w-0 flex-1 gap-1">
            <span class="flex min-w-0 items-center gap-2">
              <span class="min-w-0 truncate text-sm font-semibold text-ink" :title="project.id"><template v-for="(part, i) in parts(project.name)" :key="i"><mark v-if="part.match" class="rounded-sm bg-warning-surface px-px">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></span>
              <span v-if="project.imported" class="shrink-0 rounded-full bg-soft px-2 py-0.5 text-2xs font-medium text-accent">Added</span>
              <Icon
                v-if="selected === project.id"
                :name="Check"
                :size="16"
                class="ml-auto text-accent"
              />
            </span>
            <span v-if="project.description" class="line-clamp-2 text-xs text-muted"><template v-for="(part, i) in parts(project.description)" :key="i"><mark v-if="part.match" class="rounded-sm bg-warning-surface px-px text-ink">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></span>
            <span class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-subtle">
              <span v-for="badge in project.badges" :key="badge" class="rounded-full border border-line px-1.5 py-px font-medium text-muted">{{ badge }}</span>
              <span v-if="project.defaultBranch" class="inline-flex items-center gap-1"><Icon :name="GitBranch" :size="12" />{{ project.defaultBranch }}</span>
              <span v-if="updatedLabel(project.updatedAt)">{{ updatedLabel(project.updatedAt) }}</span>
              <a
                v-if="project.url"
                :href="project.url"
                target="_blank"
                rel="noopener noreferrer"
                class="inline-flex items-center gap-1 hover:text-accent"
                :aria-label="`Open ${project.name}`"
                @click.stop
              ><Icon :name="ExternalLink" :size="12" />Open</a>
            </span>
          </span>
        </div>
        <div v-if="projects.length && !loading && !error && !filtered.length" class="grid justify-items-center gap-2 px-4 py-8 text-center text-sm text-muted">
          <Icon :name="SearchX" :size="22" />
          <span>No project matches{{ query ? ` “${query.trim()}”` : '' }}.</span>
          <UiButton
            v-if="query"
            type="button"
            size="small"
            @click="query = ''"
          >
            Clear search
          </UiButton>
        </div>
        <div v-if="!loading && !error && !projects.length && nextPage === null" class="grid justify-items-center gap-2 px-4 py-8 text-center text-sm text-muted">
          <PluginMark :plugin="source.pluginId" :brand="source.brand" :size="28" />
          <span>No project is available with this {{ source.pluginName }} instance.</span>
        </div>
        <div v-if="nextPage !== null && projects.length" ref="sentinel" class="flex justify-center py-1.5">
          <UiButton
            type="button"
            size="small"
            :disabled="disabled || loading || !!error"
            @click="load"
          >
            {{ loading ? 'Loading projects…' : 'Load more projects' }}
          </UiButton>
        </div>
      </div>
      <p class="text-2xs text-subtle phone:hidden">
        <kbd class="rounded border border-line px-1">↑</kbd> <kbd class="rounded border border-line px-1">↓</kbd> to browse · <kbd class="rounded border border-line px-1">Enter</kbd> to choose · <kbd class="rounded border border-line px-1">Esc</kbd> to clear
      </p>
    </template>
  </section>
</template>
