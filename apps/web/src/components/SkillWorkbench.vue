<script setup lang="ts">
import type { SkillArchiveChunk, SkillBundle, SkillFileContent } from '../../../../packages/contracts/contracts'
import type { DraftFile, PickedFile } from '../skill-bundle'
import { twMerge } from 'tailwind-merge'
import {
  computed,
  nextTick,
  onMounted,
  ref,
  watch,
} from 'vue'
import {
  api,
  ApiError,
  notify,
  refresh,
  state,
} from '../api'
import {
  ArrowLeft,
  BookOpen,
  Check,
  Code,
  Download,
  Eye,
  FileBox,
  FilePlus,
  FolderGit2,
  FolderPlus,
  Github,
  Globe,
  LoaderCircle,
  Lock,
  RefreshCw,
  Terminal,
  TriangleAlert,
  Upload,
  X,
} from '../icons'
import {
  base64ToBytes,
  basename,
  countChanges,
  defaultSkillLimits,
  deletedFiles,
  dirname,
  downloadArchive,
  draftFromBundle,
  draftState,
  formatBytes,
  imageType,
  isInside,
  isMarkdown,
  joinPath,
  limitProblems,
  newSkillManifest,
  normalizePath,
  plural,
  readPickedFile,
  saveBytes,
  shortCommit,
  skillChanges,
  skillFileKind,
  skillManifest,
  stageChanges,
  syncManifestName,
  utf8Bytes,
  validateSkillPath,
} from '../skill-bundle'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import Modal from './Modal.vue'
import SkillFileTree from './SkillFileTree.vue'
import SkillSourcePanel from './SkillSourcePanel.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'
import VirtualSelect from './VirtualSelect.vue'

// Edits a whole skill folder locally — text, uploads, renames, deletions, modes — and saves
// it as one batch against the revision it loaded. Members get the same view, read-only.
const props = defineProps<{ target: { scope: string, name: string } | null }>()
const emit = defineEmits<{ close: [] }>()

const readonly = computed(() => state.installationRole !== 'owner')
const bundle = ref<SkillBundle | null>(null)
const draft = ref<DraftFile[]>([])
const baselines = new Map<string, string>()
const name = ref(props.target?.name ?? '')
const scope = ref(props.target?.scope ?? 'global')
const selectedPath = ref<string | null>(null)
const view = ref<'write' | 'preview'>('write')
const pane = ref<'file' | 'source'>('file')
const mobilePane = ref<'tree' | 'editor'>('editor')
const loading = ref(false)
const fileLoading = ref('')
const saving = ref('')
const sourceWorking = ref(false)
const addingFiles = ref(0)
const editingLocked = computed(() => readonly.value || !!saving.value || sourceWorking.value)
const downloading = ref(false)
const error = ref('')
const conflict = ref(false)
const notice = ref('')
const imageSize = ref('')
const picker = ref<HTMLInputElement>()
const pickerFolder = ref('')
const gutter = ref<HTMLElement>()
const tree = ref<InstanceType<typeof SkillFileTree>>()
const nameInput = ref<HTMLInputElement>()

const creating = computed(() => !bundle.value)
const limits = computed(() => bundle.value?.limits ?? defaultSkillLimits)
const original = computed(() => bundle.value?.files ?? [])
const deleted = computed(() => deletedFiles(original.value, draft.value))
const changes = computed(() => countChanges(original.value, draft.value))
const totalBytes = computed(() => draft.value.reduce((sum, file) => sum + file.size, 0))
const scripts = computed(() => draft.value.filter(file => file.executable).length)
const problems = computed(() => limitProblems(draft.value, limits.value, original.value))
const selected = computed(() => draft.value.find(file => file.path === selectedPath.value))
const kind = computed(() => selected.value ? skillFileKind(selected.value) : null)
const editable = computed(() => !editingLocked.value && !!selected.value?.text)
const previewable = computed(() => !!selected.value && (kind.value === 'manifest' || isMarkdown(selected.value.path) || (kind.value === 'image' && selected.value.text)))
const gutterText = computed(() => Array.from({ length: (selected.value?.content ?? '').split('\n').length }, (_, index) => index + 1).join('\n'))
const scopeLabel = computed(() => scope.value === 'global' ? 'Global' : state.projects.find(project => project.id === scope.value)?.name ?? 'Project')
const base = computed(() => `/skills/${encodeURIComponent(scope.value)}/${encodeURIComponent(name.value)}`)
const usage = computed(() => Math.min(1, totalBytes.value / limits.value.bundleBytes))
const dirty = computed(() => {
  if (!creating.value)
    return changes.value > 0
  const manifest = draft.value.find(file => file.path === skillManifest)
  return !!name.value || draft.value.length !== 1 || manifest?.content !== newSkillManifest()
})
const scopes = computed(() => [
  {
    value: 'global',
    label: 'Global',
    description: 'Available to every project',
    group: 'Workspace',
    icon: Globe,
  },
  ...state.projects.map(project => ({
    value: project.id,
    label: project.name,
    description: project.path,
    group: 'Projects',
    icon: FolderGit2,
  })),
])
const imageSource = computed(() => {
  const file = selected.value
  const type = file && imageType(file.path)
  if (!file || !type)
    return ''
  if (file.data !== undefined)
    return `data:${type};base64,${file.data}`
  if (file.content !== undefined)
    return `data:${type};charset=utf-8,${encodeURIComponent(file.content)}`
  return ''
})
const manifestParts = computed(() => {
  const content = selected.value?.content ?? ''
  const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/.exec(content)
  if (!match)
    return { fields: [] as [string, string][], body: content }
  const fields = match[1]!.split('\n').flatMap((line) => {
    const field = /^([\w-]+):(.*)$/.exec(line)
    return field ? [[field[1]!, field[2]!.trim().replace(/^["']|["']$/g, '')] as [string, string]] : []
  })
  return { fields, body: content.slice(match[0].length) }
})

// Dialog state for creating, renaming and removing files.
const creatingFile = ref<{
  folder: boolean
  parent: string
  path: string
  first: string
  executable: boolean
  touched: boolean
  error: string
}>()
const renaming = ref<{
  from: string
  folder: boolean
  to: string
  error: string
}>()
const removingFolder = ref<string>()
const closing = ref(false)

function startNew() {
  const content = newSkillManifest()
  draft.value = [{
    path: skillManifest,
    origin: null,
    size: utf8Bytes(content),
    text: true,
    executable: false,
    originExecutable: false,
    content,
    modified: true,
  }]
  baselines.clear()
  select(skillManifest)
}

function applyBundle(result: SkillBundle, carry: DraftFile[] = []) {
  bundle.value = result
  baselines.clear()
  const next = draftFromBundle(result.files)
  for (const file of next) {
    const previous = carry.find(entry => entry.path === file.path)
    if (previous?.content !== undefined && file.text && utf8Bytes(previous.content) === file.size) {
      file.content = previous.content
      baselines.set(file.path, previous.content)
    }
    else if (previous?.data !== undefined && !file.text) {
      file.data = previous.data
    }
  }

  draft.value = next
  conflict.value = false
  if (!selectedPath.value || !next.some(file => file.path === selectedPath.value))
    select(next.some(file => file.path === skillManifest) ? skillManifest : next[0]?.path ?? null)
  else
    void ensureContent(selectedPath.value)
}

async function load() {
  loading.value = true
  error.value = ''
  try {
    applyBundle(await api<SkillBundle>(`${base.value}/bundle`))
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    loading.value = false
  }
}

async function ensureContent(path: string | null) {
  const file = draft.value.find(entry => entry.path === path)
  if (!file || file.origin === null || file.content !== undefined || file.data !== undefined || !bundle.value)
    return

  const origin = file.origin
  fileLoading.value = origin
  try {
    const result = await api<SkillFileContent>(`${base.value}/file?path=${encodeURIComponent(origin)}`)
    const current = draft.value.find(entry => entry.origin === origin)
    if (!current || current.content !== undefined || current.data !== undefined)
      return
    if (result.encoding === 'utf-8') {
      current.content = result.content
      baselines.set(origin, result.content)
    }
    else {
      current.data = result.content
    }
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    if (fileLoading.value === origin)
      fileLoading.value = ''
  }
}

function select(path: string | null, openEditor = true) {
  selectedPath.value = path
  pane.value = 'file'
  imageSize.value = ''
  const file = draft.value.find(entry => entry.path === path)
  if (file) {
    const markdown = file.path === skillManifest || isMarkdown(file.path)
    const svg = skillFileKind(file) === 'image' && file.text
    view.value = svg || (markdown && readonly.value) ? 'preview' : 'write'
  }

  if (openEditor)
    mobilePane.value = 'editor'
  void ensureContent(path)
}

function editText(value: string) {
  const file = selected.value
  if (!file || editingLocked.value)
    return
  file.content = value
  file.data = undefined
  file.size = utf8Bytes(value)
  file.modified = file.origin === null || baselines.get(file.origin) !== value
}

function syncGutter(event: Event) {
  if (gutter.value)
    gutter.value.scrollTop = (event.target as HTMLTextAreaElement).scrollTop
}

function occupied(path: string, ignore: string[] = []) {
  return draft.value.find(file => !ignore.includes(file.path) && (file.path === path || isInside(file.path, path) || isInside(path, file.path)))
}

function pathProblem(path: string, ignore: string[] = []) {
  const invalid = validateSkillPath(path)
  if (invalid)
    return invalid
  const clash = occupied(path, ignore)
  if (clash)
    return clash.path === path ? `${path} already exists.` : `${clash.path} is in the way.`
  return ''
}

async function addFiles(picked: PickedFile[], folder: string) {
  if (editingLocked.value)
    return
  addingFiles.value += 1
  try {
    error.value = ''
    const skipped: string[] = []
    const oversized: string[] = []
    let added = 0
    let replaced = 0
    let last = ''
    for (const entry of picked) {
      const path = joinPath(folder, normalizePath(entry.path))
      if (validateSkillPath(path)) {
        skipped.push(entry.path)
        continue
      }

      if (entry.file.size > limits.value.fileBytes) {
        oversized.push(entry.file.name)
        continue
      }

      const existing = draft.value.find(file => file.path === path)
      if (!existing && occupied(path)) {
        skipped.push(entry.path)
        continue
      }

      const read = await readPickedFile(entry.file)
      if (path === skillManifest && !read.text) {
        skipped.push(entry.path)
        continue
      }

      if (existing) {
        Object.assign(existing, {
          size: read.size,
          text: read.text,
          content: read.content,
          data: read.data,
          modified: true,
          executable: existing.executable || read.executable,
        })
        replaced += 1
      }
      else {
        draft.value.push({
          path,
          origin: null,
          size: read.size,
          text: read.text,
          executable: read.executable,
          originExecutable: false,
          content: read.content,
          data: read.data,
          modified: true,
        })
        added += 1
      }

      last = path
    }

    const parts = []
    if (added)
      parts.push(`Added ${plural(added, 'file')}`)
    if (replaced)
      parts.push(`replaced ${plural(replaced, 'file')}`)
    if (parts.length)
      notice.value = `${parts.join(', ')}. Save to keep ${added + replaced === 1 ? 'it' : 'them'}.`
    if (oversized.length)
      error.value = `${oversized.join(', ')} ${oversized.length === 1 ? 'is' : 'are'} larger than ${formatBytes(limits.value.fileBytes)}, the limit for one file.`
    else if (skipped.length)
      error.value = `Skipped ${skipped.join(', ')}: hidden files, invalid paths and paths taken by a folder are not added.`
    if (last) {
      tree.value?.reveal(last)
      select(last)
    }
  }
  finally {
    addingFiles.value -= 1
  }
}

function openPicker(folder: string) {
  if (editingLocked.value)
    return
  pickerFolder.value = folder
  picker.value!.value = ''
  picker.value!.click()
}

function picked(event: Event) {
  const files = [...((event.target as HTMLInputElement).files ?? [])]
  void addFiles(files.map(file => ({ path: file.name, file })), pickerFolder.value)
}

function newFile(parent = selected.value ? dirname(selected.value.path) : '', folder = false) {
  if (editingLocked.value)
    return
  creatingFile.value = {
    folder,
    parent,
    path: folder ? '' : parent ? `${parent}/` : '',
    first: 'README.md',
    executable: false,
    touched: false,
    error: '',
  }
}

watch(() => creatingFile.value?.path, (path) => {
  if (creatingFile.value && !creatingFile.value.touched && !creatingFile.value.folder)
    creatingFile.value.executable = /\.(?:sh|bash|zsh|py|rb|pl)$/.test(path ?? '')
})

function createFile() {
  if (editingLocked.value)
    return
  const form = creatingFile.value!
  const path = form.folder ? joinPath(joinPath(form.parent, form.path), form.first) : normalizePath(form.path)
  form.error = pathProblem(path)
  if (form.error)
    return

  const interpreter = /\.py$/.test(path) ? 'python3' : /\.rb$/.test(path) ? 'ruby' : /\.pl$/.test(path) ? 'perl' : 'bash'
  const content = form.executable ? `#!/usr/bin/env ${interpreter}\n\n` : ''
  draft.value.push({
    path,
    origin: null,
    size: utf8Bytes(content),
    text: true,
    executable: form.executable,
    originExecutable: false,
    content,
    modified: true,
  })
  creatingFile.value = undefined
  tree.value?.reveal(path)
  select(path)
  view.value = 'write'
}

function startRename(path: string, folder: boolean) {
  if (editingLocked.value)
    return
  renaming.value = {
    from: path,
    folder,
    to: path,
    error: '',
  }
}

function move(from: string, to: string, folder: boolean) {
  if (editingLocked.value)
    return 'Wait for the save to finish.'
  const moving = folder ? draft.value.filter(file => isInside(file.path, from)) : draft.value.filter(file => file.path === from)
  if (moving.some(file => file.path === skillManifest))
    return 'SKILL.md stays at the top of the skill.'
  const ignore = moving.map(file => file.path)
  for (const file of moving) {
    const target = folder ? `${to}${file.path.slice(from.length)}` : to
    const problem = pathProblem(target, ignore)
    if (problem)
      return problem
  }

  for (const file of moving) {
    const target = folder ? `${to}${file.path.slice(from.length)}` : to
    if (selectedPath.value === file.path)
      selectedPath.value = target
    file.path = target
  }

  if (selectedPath.value)
    tree.value?.reveal(selectedPath.value)
  return ''
}

function applyRename() {
  const form = renaming.value!
  const to = normalizePath(form.to)
  if (to === form.from) {
    renaming.value = undefined
    return
  }

  form.error = move(form.from, to, form.folder)
  if (!form.error)
    renaming.value = undefined
}

function moveInto(path: string, folder: string) {
  const problem = move(path, joinPath(folder, basename(path)), false)
  if (problem)
    error.value = problem
}

function remove(path: string, folder: boolean) {
  if (editingLocked.value)
    return
  if (folder) {
    removingFolder.value = path
    return
  }

  draft.value = draft.value.filter(file => file.path !== path)
  if (selectedPath.value === path)
    select(skillManifest, false)
}

function removeFolder() {
  if (editingLocked.value)
    return
  const folder = removingFolder.value!
  draft.value = draft.value.filter(file => !isInside(file.path, folder))
  if (selectedPath.value && isInside(selectedPath.value, folder))
    select(skillManifest, false)
  removingFolder.value = undefined
}

function restore(path: string) {
  if (editingLocked.value)
    return
  const file = original.value.find(entry => entry.path === path)
  if (!file || occupied(path))
    return
  const [restored] = draftFromBundle([file])
  if (baselines.has(path))
    restored!.content = baselines.get(path)
  draft.value.push(restored!)
}

function toggleExecutable(path = selectedPath.value) {
  const file = draft.value.find(entry => entry.path === path)
  if (file && file.path !== skillManifest && !editingLocked.value)
    file.executable = !file.executable
}

async function downloadFile(path = selectedPath.value) {
  const file = draft.value.find(entry => entry.path === path)
  if (!file)
    return
  await ensureContent(file.path)
  if (file.content !== undefined)
    saveBytes(file.content, basename(file.path), 'text/plain;charset=utf-8')
  else if (file.data !== undefined)
    saveBytes(base64ToBytes(file.data), basename(file.path), imageType(file.path))
}

async function exportArchive() {
  if (!bundle.value)
    return
  downloading.value = true
  error.value = ''
  try {
    const archive = await downloadArchive(async (offset, revision) => api<SkillArchiveChunk>(`${base.value}/archive?offset=${offset}${revision ? `&revision=${encodeURIComponent(revision)}` : ''}`))
    saveBytes(archive.bytes, archive.filename, 'application/zip')
    notify(`Downloaded ${archive.filename}`)
  }
  catch (e) {
    error.value = e instanceof ApiError && e.status === 409 ? 'The skill changed during the download. Try again.' : (e as Error).message
  }
  finally {
    downloading.value = false
  }
}

async function save() {
  if (editingLocked.value || addingFiles.value)
    return
  error.value = ''
  notice.value = ''
  if (creating.value && !/^[a-z0-9][a-z0-9-]{0,63}$/.test(name.value)) {
    error.value = 'Name the skill with lowercase letters, numbers and dashes, like “release-notes”.'
    nameInput.value?.focus()
    return
  }

  if (problems.value.length) {
    error.value = problems.value[0]!.message
    return
  }

  const batch = skillChanges(original.value, draft.value)
  if (!batch.length)
    return

  saving.value = 'Saving…'
  const carry = draft.value.map(file => ({ ...file }))
  try {
    const staged = await stageChanges(batch, async (content) => {
      const upload = await api<{ id: string }>('/skills/uploads', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: new Blob([content]) })
      return upload.id
    }, {
      onProgress: (done, total) => {
        saving.value = done < total ? `Uploading ${done + 1} of ${total}…` : 'Saving…'
      },
    })
    const result = await api<SkillBundle>(`${base.value}/bundle`, {
      method: 'POST',
      body: JSON.stringify({ base: bundle.value?.revision ?? null, changes: staged }),
    })
    const created = creating.value
    applyBundle(result, carry)
    notify(created ? 'Skill created' : 'Skill saved')
    await refresh()
  }
  catch (e) {
    if (e instanceof ApiError && e.status === 409) {
      if (creating.value)
        error.value = `A skill named “${name.value}” already exists in ${scopeLabel.value}.`
      else
        conflict.value = true
    }
    else {
      error.value = (e as Error).message
    }
  }
  finally {
    saving.value = ''
  }
}

function discard() {
  if (editingLocked.value)
    return
  error.value = ''
  notice.value = ''
  if (!bundle.value) {
    name.value = ''
    startNew()
    return
  }

  const contents = [...baselines.entries()].map(([path, content]) => ({ path, content } as DraftFile))
  applyBundle(bundle.value, contents)
}

async function reload() {
  conflict.value = false
  await load()
  notice.value = 'Reloaded the latest version.'
}

function requestClose() {
  if (saving.value || sourceWorking.value || addingFiles.value)
    return
  if (dirty.value && !readonly.value && !closing.value) {
    closing.value = true
    return
  }

  emit('close')
}

function keydown(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
    event.preventDefault()
    void save()
  }
}

function replaced(result: SkillBundle) {
  applyBundle(result)
  pane.value = 'file'
  notify('Skill updated from GitHub')
  void refresh()
}

async function detached() {
  pane.value = 'file'
  notify('Source detached')
  if (bundle.value)
    bundle.value = { ...bundle.value, source: null }
  await refresh()
}

watch(name, (value) => {
  if (!creating.value)
    return
  const manifest = draft.value.find(file => file.path === skillManifest)
  if (manifest?.content !== undefined) {
    manifest.content = syncManifestName(manifest.content, value || 'my-skill')
    manifest.size = utf8Bytes(manifest.content)
  }
})

onMounted(async () => {
  if (props.target) {
    await load()
    if (draft.value.length > 1 && matchMedia('(max-width: 640px)').matches)
      mobilePane.value = 'tree'
  }
  else {
    startNew()
    await nextTick()
    nameInput.value?.focus()
  }
})
</script>

<template>
  <Modal
    :title="readonly ? 'View skill' : creating ? 'New skill' : 'Edit skill'"
    headless
    workbench
    @close="requestClose"
  >
    <div class="flex h-full min-h-0 flex-col" @keydown="keydown">
      <header class="flex items-center gap-3.5 border-b border-line px-5 py-3.5 phone:gap-2.5 phone:px-3.5 phone:py-3">
        <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-soft text-accent phone:size-9"><Icon :name="BookOpen" :size="20" /></span>
        <div class="min-w-0 flex-1">
          <h2 class="truncate text-lg leading-tight">
            {{ creating ? (name || 'New skill') : name }}
          </h2>
          <p class="m-0! mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 text-2xs leading-normal text-muted">
            <span>{{ scopeLabel }}</span>
            <span aria-hidden="true">·</span>
            <span>{{ plural(draft.length, 'file') }}</span>
            <span aria-hidden="true">·</span>
            <span>{{ formatBytes(totalBytes) }}</span>
            <template v-if="scripts">
              <span aria-hidden="true">·</span><span>{{ plural(scripts, 'script') }}</span>
            </template>
            <template v-if="readonly">
              <span aria-hidden="true">·</span><span class="inline-flex items-center gap-1"><Icon :name="Lock" :size="11" />Read only</span>
            </template>
          </p>
        </div>
        <button
          v-if="bundle?.source"
          type="button"
          class="flex min-w-0 items-center gap-2 rounded-full border border-line py-1.5 pr-3 pl-2 text-2xs transition-colors hover:bg-hover phone:hidden"
          :class="pane === 'source' ? 'bg-soft' : 'bg-surface'"
          :aria-pressed="pane === 'source'"
          aria-label="GitHub source"
          @click="pane = pane === 'source' ? 'file' : 'source'; mobilePane = 'editor'"
        >
          <Icon :name="Github" :size="15" />
          <span class="max-w-48 truncate font-medium text-ink">{{ bundle.source.repository }}</span>
          <code class="text-muted">{{ shortCommit(bundle.source.commit) }}</code>
          <span v-if="bundle.source.modified" class="size-1.5 rounded-full bg-warning" title="Edited since import" />
        </button>
        <UiButton
          v-if="bundle"
          size="small"
          class="phone:min-h-10 phone:px-2.5"
          :disabled="downloading"
          aria-label="Download .zip"
          @click="exportArchive"
        >
          <Icon :name="downloading ? LoaderCircle : Download" :size="14" :class="downloading ? 'animate-spin' : ''" /><span class="phone:hidden">Download .zip</span>
        </UiButton>
        <button
          type="button"
          :class="twMerge(iconButton, 'icon-button')"
          aria-label="Close dialog"
          @click="requestClose"
        >
          <Icon :name="X" :size="20" />
        </button>
      </header>

      <div v-if="creating" class="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-4 border-b border-line bg-surface px-5 py-3.5 phone:grid-cols-1 phone:gap-3 phone:px-3.5">
        <label>Name<input
          ref="nameInput"
          v-model="name"
          :disabled="editingLocked"
          aria-label="Skill name"
          required
          pattern="[a-z0-9][a-z0-9\-]{0,63}"
          placeholder="release-notes"
          autocomplete="off"
          spellcheck="false"
        ></label>
        <VirtualSelect v-model="scope" label="Scope" :options="scopes" />
      </div>

      <div v-if="loading && !bundle" class="grid flex-1 place-items-center text-sm text-muted">
        <span class="flex items-center gap-2"><Icon :name="LoaderCircle" :size="16" class="animate-spin" />Loading skill…</span>
      </div>

      <div v-else class="grid min-h-0 flex-1 grid-cols-[minmax(232px,280px)_minmax(0,1fr)] tablet:grid-cols-[210px_minmax(0,1fr)] phone:grid-cols-1!">
        <aside class="flex min-h-0 flex-col border-r border-line bg-inset phone:border-r-0" :class="mobilePane === 'tree' ? '' : 'phone:hidden'">
          <div class="flex items-center gap-1 px-3.5 pt-3 pb-1.5">
            <p class="m-0! flex-1 text-3xs font-semibold tracking-[0.08em] text-subtle uppercase">
              Files
            </p>
            <template v-if="!readonly">
              <button
                type="button"
                :class="twMerge(iconButton, 'size-7 phone:size-10')"
                :disabled="editingLocked"
                aria-label="New file"
                title="New file"
                @click="newFile()"
              >
                <Icon :name="FilePlus" :size="16" />
              </button>
              <button
                type="button"
                :class="twMerge(iconButton, 'size-7 phone:size-10')"
                :disabled="editingLocked"
                aria-label="New folder"
                title="New folder"
                @click="newFile(selected ? dirname(selected.path) : '', true)"
              >
                <Icon :name="FolderPlus" :size="16" />
              </button>
              <button
                type="button"
                :class="twMerge(iconButton, 'size-7 phone:size-10')"
                :disabled="editingLocked"
                aria-label="Upload files"
                title="Upload files"
                @click="openPicker('')"
              >
                <Icon :name="Upload" :size="16" />
              </button>
              <input
                ref="picker"
                type="file"
                multiple
                class="sr-only"
                tabindex="-1"
                aria-label="Upload skill files"
                @change="picked"
              >
            </template>
          </div>
          <SkillFileTree
            ref="tree"
            :files="draft"
            :deleted="deleted"
            :selected="pane === 'file' ? selectedPath : null"
            :readonly="editingLocked"
            @select="select"
            @rename="startRename"
            @remove="remove"
            @executable="toggleExecutable"
            @download="downloadFile"
            @create="(folder) => newFile(folder)"
            @upload="openPicker"
            @restore="restore"
            @drop="addFiles"
            @move="moveInto"
          />
          <p v-if="!readonly" class="m-0! px-4 pb-2 text-3xs leading-relaxed text-subtle phone:hidden">
            Drop files or folders onto the tree.
          </p>
          <button
            v-if="bundle?.source"
            type="button"
            class="mx-2.5 mb-2 flex items-center gap-2.5 rounded-xl border border-line px-3 py-2.5 text-left transition-colors hover:bg-hover"
            :class="pane === 'source' ? 'bg-soft' : 'bg-surface'"
            aria-label="Open GitHub source"
            @click="pane = 'source'; mobilePane = 'editor'"
          >
            <Icon :name="Github" :size="17" />
            <span class="min-w-0 flex-1">
              <span class="block truncate text-xs font-semibold">{{ bundle.source.repository }}</span>
              <span class="block truncate text-3xs text-muted">{{ bundle.source.ref }} · {{ shortCommit(bundle.source.commit) }}{{ bundle.source.modified ? ' · edited' : '' }}</span>
            </span>
            <span v-if="bundle.source.modified" class="size-1.5 shrink-0 rounded-full bg-warning" />
          </button>
          <div class="border-t border-line px-4 py-3">
            <div
              class="h-1 overflow-hidden rounded-full bg-variant"
              role="meter"
              aria-label="Skill size"
              :aria-valuenow="totalBytes"
              :aria-valuemax="limits.bundleBytes"
              :aria-valuetext="`${formatBytes(totalBytes)} of ${formatBytes(limits.bundleBytes)}`"
            >
              <div class="h-full rounded-full transition-[width]" :class="usage > 0.9 ? 'bg-danger' : usage > 0.7 ? 'bg-warning' : 'bg-accent'" :style="{ width: `${Math.max(usage * 100, 1.5)}%` }" />
            </div>
            <p class="m-0! mt-1.5 flex justify-between text-3xs tabular-nums text-subtle">
              <span>{{ formatBytes(totalBytes) }} of {{ formatBytes(limits.bundleBytes) }}</span><span>{{ draft.length }} / {{ limits.files }} files</span>
            </p>
          </div>
        </aside>

        <section class="flex min-h-0 min-w-0 flex-col bg-raised" :class="mobilePane === 'editor' ? '' : 'phone:hidden'">
          <template v-if="pane === 'source' && bundle?.source">
            <div class="flex items-center gap-2 border-b border-line px-4 py-2.5">
              <button
                type="button"
                :class="twMerge(iconButton, 'hidden phone:inline-flex')"
                aria-label="Back to files"
                @click="mobilePane = 'tree'"
              >
                <Icon :name="ArrowLeft" :size="18" />
              </button>
              <p class="m-0! flex-1 text-sm font-semibold">
                Source
              </p>
              <button
                type="button"
                :class="twMerge(iconButton, 'size-8')"
                aria-label="Close source"
                @click="pane = 'file'"
              >
                <Icon :name="X" :size="16" />
              </button>
            </div>
            <SkillSourcePanel
              :scope="scope"
              :name="name"
              :revision="bundle.revision"
              :source="bundle.source"
              :unsaved="changes"
              :readonly="editingLocked"
              @working="sourceWorking = $event"
              @replaced="replaced"
              @detached="detached"
              @conflict="conflict = true"
            />
          </template>

          <template v-else-if="selected">
            <div class="flex min-h-13 items-center gap-2 border-b border-line px-4 py-2 phone:flex-wrap phone:px-3">
              <button
                type="button"
                :class="twMerge(iconButton, 'hidden phone:inline-flex')"
                aria-label="Back to files"
                @click="mobilePane = 'tree'"
              >
                <Icon :name="ArrowLeft" :size="18" />
              </button>
              <div class="flex min-w-0 flex-1 items-baseline gap-1 overflow-hidden font-mono text-xs">
                <span v-if="dirname(selected.path)" class="truncate text-subtle">{{ dirname(selected.path) }}/</span>
                <span class="shrink-0 font-semibold text-ink">{{ basename(selected.path) }}</span>
                <span class="ml-2 shrink-0 font-sans text-3xs text-subtle tabular-nums phone:hidden">{{ formatBytes(selected.size) }}</span>
                <span
                  v-if="draftState(selected)"
                  class="ml-1 shrink-0 rounded-[4px] px-1.5 py-px font-sans text-3xs font-semibold phone:hidden"
                  :class="draftState(selected) === 'new' ? 'bg-success/12 text-success' : 'bg-warning-surface text-warning'"
                >{{ { new: 'New', modified: 'Modified', moved: 'Renamed', mode: 'Mode changed' }[draftState(selected)!] }}</span>
              </div>
              <button
                v-if="selected.path !== skillManifest && !readonly"
                type="button"
                role="switch"
                :aria-checked="selected.executable"
                :disabled="editingLocked"
                aria-label="Executable"
                class="flex min-h-8 items-center gap-2 rounded-lg px-2 text-2xs font-medium transition-colors hover:bg-hover phone:min-h-10"
                :class="selected.executable ? 'text-success' : 'text-muted'"
                @click="toggleExecutable()"
              >
                <Icon :name="Terminal" :size="14" />Executable
                <span class="relative h-4 w-7 rounded-full transition-colors" :class="selected.executable ? 'bg-success' : 'bg-control'">
                  <span class="absolute top-0.5 size-3 rounded-full bg-white shadow-sm transition-[left]" :class="selected.executable ? 'left-3.5' : 'left-0.5'" />
                </span>
              </button>
              <span v-else-if="selected.executable" class="flex items-center gap-1.5 px-2 text-2xs font-medium text-success"><Icon :name="Terminal" :size="14" />Executable</span>
              <UiSegments
                v-if="previewable"
                v-model="view"
                label="Skill editor view"
                compact
                :options="[{ value: 'write', label: readonly ? 'Source' : 'Write', icon: Code }, { value: 'preview', label: 'Preview', icon: Eye }]"
              />
              <button
                type="button"
                :class="twMerge(iconButton, 'size-8')"
                aria-label="Download file"
                title="Download file"
                @click="downloadFile()"
              >
                <Icon :name="Download" :size="16" />
              </button>
            </div>

            <div v-if="fileLoading && fileLoading === selected.origin" class="grid flex-1 place-items-center text-sm text-muted">
              <span class="flex items-center gap-2"><Icon :name="LoaderCircle" :size="16" class="animate-spin" />Loading {{ basename(selected.path) }}…</span>
            </div>

            <div v-else-if="kind === 'image' && (!selected.text || view === 'preview')" class="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 overflow-auto p-8 phone:p-4">
              <div class="checkerboard grid max-h-full max-w-full place-items-center overflow-hidden rounded-xl border border-line p-3 shadow-arcade">
                <img
                  v-if="imageSource"
                  :src="imageSource"
                  :alt="selected.path"
                  class="max-h-[56dvh] max-w-full object-contain [image-rendering:auto]"
                  @load="imageSize = `${($event.target as HTMLImageElement).naturalWidth} × ${($event.target as HTMLImageElement).naturalHeight}`"
                >
              </div>
              <p class="m-0! text-2xs text-muted tabular-nums">
                {{ imageSize ? `${imageSize} px · ` : '' }}{{ formatBytes(selected.size) }} · {{ imageType(selected.path) }}
              </p>
            </div>

            <div v-else-if="!selected.text" class="grid flex-1 place-items-center p-8">
              <div class="flex max-w-80 flex-col items-center text-center">
                <span class="mb-4 grid size-16 place-items-center rounded-[19px] border border-line bg-surface text-muted"><Icon :name="FileBox" :size="28" /></span>
                <p class="m-0! font-heading text-base font-[650] break-all">
                  {{ basename(selected.path) }}
                </p>
                <p class="m-0! mt-1 text-xs text-muted">
                  Binary file · {{ formatBytes(selected.size) }}{{ selected.executable ? ' · executable' : '' }}
                </p>
                <p class="m-0! mt-3 text-xs leading-relaxed text-muted">
                  Agents receive this file as is. Download it to inspect, or upload a replacement.
                </p>
                <div class="mt-5 flex gap-2.5">
                  <UiButton size="small" @click="downloadFile()">
                    <Icon :name="Download" :size="14" />Download
                  </UiButton>
                  <UiButton v-if="!readonly" size="small" @click="openPicker(dirname(selected.path))">
                    <Icon :name="Upload" :size="14" />Replace
                  </UiButton>
                </div>
              </div>
            </div>

            <div v-else-if="view === 'preview' && previewable" class="min-h-0 flex-1 overflow-auto overscroll-contain">
              <div class="skill-preview mx-auto max-w-[760px] px-8 py-7 phone:px-4 phone:py-5">
                <dl v-if="kind === 'manifest' && manifestParts.fields.length" class="m-0! mb-6 grid grid-cols-[auto_minmax(0,1fr)] gap-x-5 gap-y-2 rounded-xl border border-line bg-surface px-4 py-3.5 text-xs">
                  <template v-for="[key, value] in manifestParts.fields" :key="key">
                    <dt class="font-mono text-subtle">
                      {{ key }}
                    </dt><dd class="m-0 text-ink">
                      {{ value }}
                    </dd>
                  </template>
                </dl>
                <Markdown :content="kind === 'manifest' ? manifestParts.body : selected.content ?? ''" />
              </div>
            </div>

            <div v-else class="relative flex min-h-0 flex-1 bg-inset">
              <div ref="gutter" class="code-gutter w-11 shrink-0 overflow-hidden border-r border-line py-3.5 pr-2.5 text-right font-mono text-xs leading-[1.8] text-subtle/60 select-none phone:hidden" aria-hidden="true">
                <pre class="m-0 font-mono text-[length:inherit] leading-[inherit]">{{ gutterText }}</pre>
              </div>
              <textarea
                :value="selected.content ?? ''"
                class="code-editor min-h-0! flex-1 resize-none! rounded-none! border-0! bg-transparent! px-4! py-3.5! font-mono! text-xs! leading-[1.8]! whitespace-pre shadow-none! [tab-size:2] focus:shadow-none!"
                wrap="off"
                spellcheck="false"
                autocapitalize="off"
                autocomplete="off"
                :readonly="!editable"
                :aria-label="selected.path === skillManifest ? 'Skill content' : 'Supporting file content'"
                @input="editText(($event.target as HTMLTextAreaElement).value)"
                @scroll="syncGutter"
              />
            </div>
          </template>

          <div v-else class="grid flex-1 place-items-center p-8 text-center text-sm text-muted">
            Select a file to view it.
          </div>
        </section>
      </div>

      <div v-if="conflict" class="flex items-center gap-3 border-t border-line bg-warning-surface px-5 py-3 text-xs text-warning phone:flex-wrap phone:px-3.5" role="alert">
        <Icon :name="TriangleAlert" :size="17" class="shrink-0" />
        <span class="min-w-0 flex-1 leading-relaxed"><strong class="font-semibold">This skill changed since you opened it.</strong> Reload to get the latest version; your unsaved edits here will be discarded.</span>
        <UiButton size="small" @click="reload">
          <Icon :name="RefreshCw" :size="14" />Reload
        </UiButton>
      </div>
      <div v-else-if="error" class="border-t border-line px-5 phone:px-3.5">
        <UiAlert class="my-2.5">
          {{ error }}
        </UiAlert>
      </div>

      <footer class="flex items-center gap-2.5 border-t border-line bg-surface px-5 py-3 phone:flex-wrap phone:px-3.5">
        <p class="m-0! flex min-w-0 flex-1 items-center gap-2 text-xs text-muted" role="status">
          <template v-if="saving">
            <Icon :name="LoaderCircle" :size="15" class="animate-spin" />{{ saving }}
          </template>
          <template v-else-if="readonly">
            <Icon :name="Lock" :size="14" />Only the owner can edit skills.
          </template>
          <template v-else-if="creating">
            <span class="size-2 rounded-full bg-accent" />Not saved yet
          </template>
          <template v-else-if="changes">
            <span class="size-2 rounded-full bg-warning" /><span class="font-medium text-ink">{{ plural(changes, 'unsaved change') }}</span>
            <button type="button" class="text-xs font-medium text-accent hover:underline" @click="discard">
              Discard
            </button>
          </template>
          <template v-else>
            <Icon :name="Check" :size="14" class="text-success" /><span class="truncate">{{ notice || 'All changes saved' }}</span>
          </template>
        </p>
        <UiButton :disabled="!!saving || sourceWorking || !!addingFiles" @click="requestClose">
          {{ dirty && !readonly ? 'Cancel' : 'Close' }}
        </UiButton>
        <UiButton
          v-if="!readonly"
          variant="primary"
          :disabled="editingLocked || !!addingFiles || (!creating && !changes) || conflict"
          @click="save"
        >
          <Icon
            v-if="saving"
            :name="LoaderCircle"
            :size="15"
            class="animate-spin"
          />Save skill
        </UiButton>
      </footer>
    </div>

    <Modal v-if="creatingFile" :title="creatingFile.folder ? 'New folder' : 'New file'" @close="creatingFile = undefined">
      <form @submit.prevent="createFile">
        <div class="modal-body grid gap-4.5 px-6.5 py-6 phone:p-5">
          <template v-if="creatingFile.folder">
            <label>Folder<input
              v-model="creatingFile.path"
              aria-label="Folder name"
              required
              autofocus
              placeholder="references"
              spellcheck="false"
            ><small v-if="creatingFile.parent">Inside {{ creatingFile.parent }}/</small></label>
            <label>First file<input
              v-model="creatingFile.first"
              aria-label="First file name"
              required
              spellcheck="false"
            ><small>Folders hold at least one file.</small></label>
          </template>
          <label v-else>Path<input
            v-model="creatingFile.path"
            aria-label="New file path"
            required
            autofocus
            placeholder="scripts/check.sh"
            spellcheck="false"
          ><small>Use slashes for folders, like references/checklist.md.</small></label>
          <label v-if="!creatingFile.folder" class="checkbox flex-row items-center gap-2 text-xs font-normal"><input v-model="creatingFile.executable" type="checkbox" @change="creatingFile.touched = true">Executable script</label>
          <p v-if="creatingFile.error" class="m-0! text-xs text-danger" role="alert">
            {{ creatingFile.error }}
          </p>
        </div>
        <footer class="modal-actions flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
          <UiButton @click="creatingFile = undefined">
            Cancel
          </UiButton>
          <UiButton variant="primary" type="submit">
            {{ creatingFile.folder ? 'Create folder' : 'Create file' }}
          </UiButton>
        </footer>
      </form>
    </Modal>

    <Modal v-if="renaming" :title="renaming.folder ? 'Rename folder' : 'Rename or move'" @close="renaming = undefined">
      <form @submit.prevent="applyRename">
        <div class="modal-body grid gap-4 px-6.5 py-6 phone:p-5">
          <label>{{ renaming.folder ? 'Folder path' : 'New path' }}<input
            v-model="renaming.to"
            aria-label="New path"
            required
            autofocus
            spellcheck="false"
          ><small>Change folders by editing the path, like references/{{ basename(renaming.from) }}.</small></label>
          <p v-if="renaming.error" class="m-0! text-xs text-danger" role="alert">
            {{ renaming.error }}
          </p>
        </div>
        <footer class="modal-actions flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
          <UiButton @click="renaming = undefined">
            Cancel
          </UiButton>
          <UiButton variant="primary" type="submit">
            Rename
          </UiButton>
        </footer>
      </form>
    </Modal>

    <Modal v-if="removingFolder" title="Delete this folder?" @close="removingFolder = undefined">
      <div class="modal-body px-6.5 py-6 phone:p-5">
        <p class="m-0!">
          {{ plural(draft.filter(file => isInside(file.path, removingFolder!)).length, 'file') }} in {{ removingFolder }}/ will be removed when you save.
        </p>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <UiButton @click="removingFolder = undefined">
          Cancel
        </UiButton>
        <UiButton variant="danger" @click="removeFolder">
          Delete folder
        </UiButton>
      </footer>
    </Modal>

    <Modal v-if="closing" title="Discard unsaved changes?" @close="closing = false">
      <div class="modal-body px-6.5 py-6 phone:p-5">
        <p class="m-0!">
          {{ creating ? 'This skill has not been saved.' : `You have ${plural(changes, 'unsaved change')}.` }} Closing the editor discards {{ creating ? 'it' : 'them' }}.
        </p>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <UiButton @click="closing = false">
          Keep editing
        </UiButton>
        <UiButton variant="danger" @click="closing = false; emit('close')">
          Discard changes
        </UiButton>
      </footer>
    </Modal>
  </Modal>
</template>

<style scoped>
.checkerboard {
  background-color: var(--color-surface);
  background-image:
    linear-gradient(45deg, var(--color-variant) 25%, transparent 25%),
    linear-gradient(-45deg, var(--color-variant) 25%, transparent 25%),
    linear-gradient(45deg, transparent 75%, var(--color-variant) 75%),
    linear-gradient(-45deg, transparent 75%, var(--color-variant) 75%);
  background-size: 16px 16px;
  background-position: 0 0, 0 8px, 8px -8px, -8px 0;
}
.code-editor {
  outline: none;
}
</style>
