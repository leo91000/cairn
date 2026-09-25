<script setup lang="ts">
import type { GithubRepository, GithubRepositoryPage } from '../../shared/contracts'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { api } from '../api'
import {
  Archive,
  Check,
  GitBranch,
  GitFork,
  Github,
  Globe,
  Lock,
  Pencil,
  RefreshCw,
  Search,
  SearchX,
  Star,
  X,
} from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

const props = defineProps<{ disabled?: boolean, selected: string }>()
const emit = defineEmits<{ select: [repository: GithubRepository] }>()
const repositories = ref<GithubRepository[]>([])
const query = ref('')
const visibility = ref<'all' | 'private' | 'public'>('all')
const loading = ref(false)
const error = ref('')
const nextPage = ref<number | null>(1)
const browsing = ref(true)
const active = ref(-1)
const search = ref<HTMLInputElement>()
const list = ref<HTMLElement>()
const sentinel = ref<HTMLElement>()
const needle = computed(() => query.value.trim().toLowerCase())
const visible = computed(() => repositories.value.filter(repo => visibility.value === 'all' || repo.private === (visibility.value === 'private')))
const filtered = computed(() => visible.value.filter(repo => `${repo.fullName} ${repo.description} ${repo.language}`.toLowerCase().includes(needle.value)))
const chosen = computed(() => repositories.value.find(repo => repo.fullName === props.selected))
const counts = computed(() => ({ all: repositories.value.length, private: repositories.value.filter(repo => repo.private).length }))
const filters = computed(() => [
  { value: 'all' as const, label: 'All', count: counts.value.all },
  {
    value: 'private' as const,
    label: 'Private',
    icon: Lock,
    count: counts.value.private,
  },
  {
    value: 'public' as const,
    label: 'Public',
    icon: Globe,
    count: counts.value.all - counts.value.private,
  },
])
// Searching and filtering keep fetching pages until enough matches are visible.
const needsMore = computed(() => nextPage.value !== null && !error.value && (!!needle.value || visibility.value !== 'all') && filtered.value.length < 8)

async function load() {
  if (loading.value || nextPage.value === null)
    return
  loading.value = true
  error.value = ''
  try {
    const page = await api<GithubRepositoryPage>(`/github/repositories?page=${nextPage.value}`)
    const known = new Set(repositories.value.map(repo => repo.fullName))
    repositories.value.push(...page.repositories.filter(repo => !known.has(repo.fullName)))
    nextPage.value = page.nextPage
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    loading.value = false
  }

  if (needsMore.value)
    await load()
}

function select(repo: GithubRepository) {
  if (props.disabled || repo.imported)
    return
  emit('select', repo)
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
  nextTick(() => list.value?.querySelector(`#github-repo-${index}`)?.scrollIntoView({ block: 'nearest' }))
}

function keydown(event: KeyboardEvent) {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    move(event.key === 'ArrowDown' ? 1 : -1)
  }
  else if (event.key === 'Enter') {
    event.preventDefault()
    const repo = filtered.value[active.value] ?? (filtered.value.filter(repo => !repo.imported).length === 1 ? filtered.value.find(repo => !repo.imported) : undefined)
    if (repo)
      select(repo)
  }
  else if (event.key === 'Escape' && query.value) {
    event.stopPropagation()
    query.value = ''
  }
}

function owner(repo: GithubRepository) {
  return repo.owner || repo.fullName.split('/')[0]!
}

function reload() {
  repositories.value = []
  nextPage.value = 1
  load()
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

const languageColors: Record<string, string> = {
  'TypeScript': '#3178c6',
  'JavaScript': '#f1e05a',
  'Rust': '#dea584',
  'Kotlin': '#a97bff',
  'Python': '#3572a5',
  'Go': '#00add8',
  'Vue': '#41b883',
  'Java': '#b07219',
  'Swift': '#f05138',
  'Ruby': '#701516',
  'PHP': '#4f5d95',
  'Shell': '#89e051',
  'HTML': '#e34c26',
  'CSS': '#663399',
  'C#': '#178600',
  'C++': '#f34b7d',
  'C': '#555555',
  'Dart': '#00b4ab',
}

function languageColor(language: string) {
  return languageColors[language] ?? `oklch(0.65 0.12 ${hue(language)})`
}

const relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' })

function updated(value: string) {
  const time = Date.parse(value)
  if (Number.isNaN(time))
    return ''
  const seconds = (time - Date.now()) / 1000
  for (const [unit, size] of [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60]] as const) {
    if (Math.abs(seconds) >= size)
      return `Updated ${relative.format(Math.round(seconds / size), unit)}`
  }

  return 'Updated just now'
}

function stars(value: number) {
  return value >= 1000 ? `${(value / 1000).toFixed(value >= 10000 ? 0 : 1)}k` : `${value}`
}

watch([needle, visibility], () => {
  active.value = -1
  list.value?.scrollTo({ top: 0 })
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
  <section class="grid min-w-0 gap-3" aria-label="GitHub repositories" :aria-busy="loading">
    <div v-if="chosen && !browsing" class="flex min-w-0 items-center gap-3 rounded-xl border border-accent bg-soft px-3.5 py-3 shadow-arcade">
      <span class="grid size-10 shrink-0 place-items-center rounded-lg text-sm font-semibold text-white" :style="{ background: `oklch(0.58 0.14 ${hue(owner(chosen))})` }" aria-hidden="true">{{ owner(chosen).slice(0, 1).toUpperCase() }}</span>
      <div class="grid min-w-0 flex-1 gap-0.5">
        <span class="flex min-w-0 items-center gap-1.5 text-sm font-semibold text-ink"><Icon :name="Check" :size="15" class="text-accent" /><span class="min-w-0 break-all">{{ chosen.fullName }}</span></span>
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
          <Icon :name="Github" :size="15" class="text-ink" />
          <span>Pick a repository from your GitHub connection. It is cloned when you save.</span>
        </p>
        <button
          type="button"
          :class="iconButton"
          :disabled="disabled || loading"
          aria-label="Reload repositories"
          title="Reload repositories"
          @click="reload"
        >
          <Icon :name="RefreshCw" :size="15" :class="loading ? 'animate-spin' : ''" />
        </button>
      </div>
      <div class="flex min-w-0 flex-wrap items-center gap-2">
        <label class="search-field flex min-w-0 flex-1 basis-56 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle">
          <Icon :name="Search" :size="16" />
          <input
            ref="search"
            v-model="query"
            type="text"
            enterkeyhint="search"
            autocomplete="off"
            spellcheck="false"
            role="combobox"
            aria-label="Search repositories"
            aria-autocomplete="list"
            aria-expanded="true"
            aria-controls="github-repositories"
            :aria-activedescendant="active >= 0 ? `github-repo-${active}` : undefined"
            placeholder="Search by owner, name, language…"
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
        <UiSegments
          v-model="visibility"
          label="Repository visibility"
          compact
          :options="filters"
        />
      </div>
      <div v-if="error" role="alert" class="flex min-w-0 flex-wrap items-center gap-3 rounded-lg border border-line bg-danger-surface px-4 py-3 text-sm text-danger">
        <span class="min-w-0 flex-1">{{ error }} <RouterLink to="/connections" class="underline">Connections</RouterLink></span>
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
        id="github-repositories"
        ref="list"
        class="grid max-h-80 min-w-0 content-start overflow-y-auto overscroll-contain rounded-xl border border-line bg-inset p-1.5"
        role="listbox"
        aria-label="Available repositories"
      >
        <template v-if="!repositories.length && loading">
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
          v-for="(repo, index) in filtered"
          :id="`github-repo-${index}`"
          :key="repo.fullName"
          role="option"
          :aria-selected="selected === repo.fullName"
          :aria-disabled="disabled || repo.imported"
          class="group flex min-w-0 cursor-pointer items-start gap-3 rounded-lg border px-2.5 py-2.5 transition-colors aria-disabled:cursor-not-allowed aria-disabled:opacity-55"
          :class="selected === repo.fullName ? 'border-accent bg-soft' : active === index ? 'border-line bg-surface shadow-arcade' : 'border-transparent'"
          @mouseenter="active = index"
          @click="select(repo)"
        >
          <span class="grid size-9 shrink-0 place-items-center rounded-lg text-sm font-semibold text-white" :style="{ background: `oklch(0.58 0.14 ${hue(owner(repo))})` }" aria-hidden="true">{{ owner(repo).slice(0, 1).toUpperCase() }}</span>
          <span class="grid min-w-0 flex-1 gap-1">
            <span class="flex min-w-0 items-center gap-2">
              <span class="min-w-0 truncate text-sm"><span class="text-muted"><template v-for="(part, i) in parts(owner(repo))" :key="i"><mark v-if="part.match" class="rounded-sm bg-warning-surface px-px text-ink">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template>/</span><span class="font-semibold text-ink"><template v-for="(part, i) in parts(repo.name)" :key="i"><mark v-if="part.match" class="rounded-sm bg-warning-surface px-px">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></span></span>
              <span v-if="repo.imported" class="shrink-0 rounded-full bg-soft px-2 py-0.5 text-2xs font-medium text-accent">Added</span>
              <Icon
                v-if="selected === repo.fullName"
                :name="Check"
                :size="16"
                class="ml-auto text-accent"
              />
            </span>
            <span v-if="repo.description" class="line-clamp-2 text-xs text-muted"><template v-for="(part, i) in parts(repo.description)" :key="i"><mark v-if="part.match" class="rounded-sm bg-warning-surface px-px text-ink">{{ part.text }}</mark><template v-else>{{ part.text }}</template></template></span>
            <span class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-subtle">
              <span class="inline-flex items-center gap-1"><Icon :name="repo.private ? Lock : Globe" :size="12" />{{ repo.private ? 'Private' : 'Public' }}</span>
              <span v-if="repo.language" class="inline-flex items-center gap-1"><span class="size-2 rounded-full" :style="{ background: languageColor(repo.language) }" />{{ repo.language }}</span>
              <span v-if="repo.stars" class="inline-flex items-center gap-1"><Icon :name="Star" :size="12" />{{ stars(repo.stars) }}</span>
              <span v-if="repo.defaultBranch" class="inline-flex items-center gap-1"><Icon :name="GitBranch" :size="12" />{{ repo.defaultBranch }}</span>
              <span v-if="repo.fork" class="inline-flex items-center gap-1"><Icon :name="GitFork" :size="12" />Fork</span>
              <span v-if="repo.archived" class="inline-flex items-center gap-1 text-warning"><Icon :name="Archive" :size="12" />Archived</span>
              <span v-if="updated(repo.pushedAt)">{{ updated(repo.pushedAt) }}</span>
            </span>
          </span>
        </div>
        <div v-if="repositories.length && !loading && !error && !filtered.length" class="grid justify-items-center gap-2 px-4 py-8 text-center text-sm text-muted">
          <Icon :name="SearchX" :size="22" />
          <span>No repository matches{{ query ? ` “${query.trim()}”` : '' }}.</span>
          <UiButton
            v-if="query || visibility !== 'all'"
            type="button"
            size="small"
            @click="query = ''; visibility = 'all'"
          >
            Clear filters
          </UiButton>
        </div>
        <div v-if="!loading && !error && !repositories.length && nextPage === null" class="grid justify-items-center gap-2 px-4 py-8 text-center text-sm text-muted">
          <Icon :name="Github" :size="22" />
          <span>No repository is available with this GitHub connection.</span>
        </div>
        <div v-if="nextPage !== null && repositories.length" ref="sentinel" class="flex justify-center py-1.5">
          <UiButton
            type="button"
            size="small"
            :disabled="disabled || loading || !!error"
            @click="load"
          >
            {{ loading ? 'Loading repositories…' : 'Load more repositories' }}
          </UiButton>
        </div>
      </div>
      <p class="text-2xs text-subtle phone:hidden">
        <kbd class="rounded border border-line px-1">↑</kbd> <kbd class="rounded border border-line px-1">↓</kbd> to browse · <kbd class="rounded border border-line px-1">Enter</kbd> to choose · <kbd class="rounded border border-line px-1">Esc</kbd> to clear
      </p>
    </template>
  </section>
</template>
