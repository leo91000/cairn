<script setup lang="ts">
import type { SkillBundle, SkillImport } from '../../../../packages/contracts/contracts'
import { computed, ref, watch } from 'vue'
import { api, ApiError, state } from '../api'
import {
  BookOpen,
  Check,
  FileArchive,
  Folder,
  FolderGit2,
  Github,
  Globe,
  LoaderCircle,
  Upload,
} from '../icons'
import {
  defaultSkillLimits,
  formatBytes,
  parseGithubSource,
} from '../skill-bundle'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'
import VirtualSelect from './VirtualSelect.vue'

// Imports a skill from a .zip/.skill archive or a GitHub folder. When the source holds
// several skills, the user picks one and the same upload is submitted again.
const emit = defineEmits<{ close: [], imported: [bundle: SkillBundle] }>()
const source = ref<'archive' | 'github'>('archive')
const scope = ref('global')
const archive = ref<File>()
const upload = ref<{ id: string, file: File }>()
const repository = ref('')
const path = ref('')
const ref_ = ref('')
const candidates = ref<string[]>([])
const candidate = ref('')
const busy = ref('')
const error = ref('')
const dragging = ref(false)
const picker = ref<HTMLInputElement>()
const limit = defaultSkillLimits.uploadBytes
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
const ready = computed(() => source.value === 'archive' ? !!archive.value : !!parseGithubSource(repository.value))

watch([source, scope, repository, path, ref_], () => {
  candidates.value = []
  candidate.value = ''
  error.value = ''
})

function choose(file: File | undefined) {
  if (busy.value)
    return
  error.value = ''
  candidates.value = []
  candidate.value = ''
  upload.value = undefined
  if (!file)
    return
  if (!/\.(?:zip|skill)$/i.test(file.name)) {
    error.value = 'Choose a .zip or .skill archive.'
    return
  }

  if (file.size > limit) {
    error.value = `${file.name} is ${formatBytes(file.size)}; archives are limited to ${formatBytes(limit)}.`
    return
  }

  archive.value = file
}

function dropped(event: DragEvent) {
  dragging.value = false
  choose(event.dataTransfer?.files[0])
}

// GitHub URLs fill in the ref and folder they name.
function pasteRepository() {
  const parsed = parseGithubSource(repository.value)
  if (!parsed || parsed.repository === repository.value)
    return
  repository.value = parsed.repository
  if (parsed.ref && !ref_.value)
    ref_.value = parsed.ref
  if (parsed.path && !path.value)
    path.value = parsed.path
}

function pasted() {
  setTimeout(pasteRepository)
}

async function submit() {
  if (!ready.value || busy.value)
    return
  error.value = ''
  pasteRepository()
  const kind = source.value
  const destination = scope.value
  const submittedArchive = archive.value
  const folder = candidate.value || (kind === 'github' ? path.value.trim().replace(/^\/+|\/+$/g, '') : '')
  const requestedRef = ref_.value.trim()
  const requestedRepository = repository.value
  busy.value = kind === 'archive' ? 'Uploading archive…' : 'Fetching from GitHub…'
  try {
    let body: Record<string, string>
    if (kind === 'archive') {
      // Uploads are consumed only by a successful import, so a candidate pick reuses this one.
      if (!upload.value || upload.value.file !== submittedArchive) {
        busy.value = 'Uploading archive…'
        const staged = await api<{ id: string }>('/skills/uploads', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: submittedArchive })
        upload.value = { id: staged.id, file: submittedArchive! }
      }

      body = { source: 'archive', upload: upload.value.id }
    }
    else {
      pasteRepository()
      body = { source: 'github', repository: parseGithubSource(requestedRepository)!.repository }
      if (requestedRef)
        body.ref = requestedRef
    }

    if (folder)
      body.path = folder

    busy.value = kind === 'github' ? 'Fetching from GitHub…' : 'Importing…'
    const result = await api<SkillImport>(`/skills/${encodeURIComponent(destination)}/import`, { method: 'POST', body: JSON.stringify(body) })
    if (result.imported) {
      emit('imported', result.imported)
      return
    }

    candidates.value = result.candidates
    candidate.value = ''
  }
  catch (e) {
    error.value = e instanceof ApiError && e.status === 409 && !/exist/i.test(e.message)
      ? `${e.message} A skill with this name may already exist in this scope.`
      : (e as Error).message
    if (e instanceof ApiError && e.status === 404 && source.value === 'archive')
      upload.value = undefined
  }
  finally {
    busy.value = ''
  }
}
</script>

<template>
  <Modal title="Import a skill" wide @close="!busy && emit('close')">
    <form @submit.prevent="submit">
      <fieldset :disabled="!!busy" class="contents">
        <div class="modal-body grid gap-5 px-6.5 py-6 phone:p-5">
          <UiSegments
            v-model="source"
            label="Import from"
            :options="[{ value: 'archive', label: 'Archive', icon: FileArchive }, { value: 'github', label: 'GitHub', icon: Github }]"
          />

          <template v-if="source === 'archive'">
            <div
              class="relative flex flex-col items-center justify-center rounded-2xl border-[1.5px] border-dashed px-6 py-8 text-center transition-colors phone:py-6"
              :class="dragging ? 'border-accent bg-soft' : archive ? 'border-line bg-surface' : 'border-control bg-inset'"
              @dragover.prevent="dragging = true"
              @dragleave="dragging = false"
              @drop.prevent="dropped"
            >
              <template v-if="archive">
                <span class="mb-3 grid size-12 place-items-center rounded-2xl bg-soft text-accent"><Icon :name="FileArchive" :size="24" /></span>
                <p class="m-0! max-w-full truncate text-sm font-semibold">
                  {{ archive.name }}
                </p>
                <p class="m-0! mt-0.5 text-xs text-muted">
                  {{ formatBytes(archive.size) }}{{ upload ? ' · uploaded' : '' }}
                </p>
                <button type="button" class="mt-3 text-xs font-medium text-accent hover:underline" @click="picker!.click()">
                  Choose another archive
                </button>
              </template>
              <template v-else>
                <span class="mb-3 grid size-12 place-items-center rounded-2xl border border-line bg-surface text-muted"><Icon :name="Upload" :size="22" /></span>
                <p class="m-0! text-sm font-semibold">
                  Drop a .zip or .skill archive
                </p>
                <p class="m-0! mt-1 text-xs text-muted">
                  Up to {{ formatBytes(limit) }} · or
                  <button type="button" class="font-medium text-accent hover:underline" @click="picker!.click()">
                    browse your files
                  </button>
                </p>
              </template>
              <input
                ref="picker"
                type="file"
                accept=".zip,.skill,application/zip"
                class="sr-only"
                tabindex="-1"
                aria-label="Skill archive"
                @change="choose(($event.target as HTMLInputElement).files?.[0])"
              >
            </div>
          </template>

          <template v-else>
            <label>Repository<input
              v-model="repository"
              aria-label="Repository"
              placeholder="owner/repo or a GitHub URL"
              autocomplete="off"
              spellcheck="false"
              @blur="pasteRepository"
              @paste="pasted"
            ><small>Uses this installation’s GitHub connection, so private repositories work too.</small></label>
            <div class="grid grid-cols-[minmax(0,2fr)_minmax(0,1fr)] gap-4 phone:grid-cols-1">
              <label>Folder <span class="font-normal text-subtle">(optional)</span><input
                v-model="path"
                aria-label="Folder in repository"
                placeholder="skills/review"
                autocomplete="off"
                spellcheck="false"
              ></label>
              <label>Branch, tag or commit <span class="font-normal text-subtle">(optional)</span><input
                v-model="ref_"
                aria-label="Ref"
                placeholder="default branch"
                autocomplete="off"
                spellcheck="false"
              ></label>
            </div>
          </template>

          <VirtualSelect
            v-model="scope"
            label="Add to"
            :options="scopes"
            :disabled="!!busy"
          />

          <section v-if="candidates.length" class="grid gap-2.5" aria-label="Skills found">
            <div>
              <p class="m-0! text-sm font-semibold">
                {{ candidates.length }} skills found
              </p>
              <p class="m-0! text-xs text-muted">
                Choose the one to import.
              </p>
            </div>
            <div class="grid max-h-64 gap-1.5 overflow-auto overscroll-contain" role="radiogroup" aria-label="Skill folder">
              <label
                v-for="item in candidates"
                :key="item"
                class="flex cursor-pointer flex-row! items-center gap-3 rounded-xl border px-3.5 py-3 transition-colors"
                :class="candidate === item ? 'border-accent bg-soft' : 'border-line bg-surface hover:bg-hover/60'"
              >
                <input
                  v-model="candidate"
                  type="radio"
                  name="candidate"
                  :value="item"
                  class="sr-only"
                >
                <span class="grid size-8 shrink-0 place-items-center rounded-lg" :class="candidate === item ? 'bg-accent text-surface' : 'bg-variant text-muted'"><Icon :name="candidate === item ? Check : BookOpen" :size="15" /></span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-sm font-semibold text-ink">{{ item.split('/').pop() || item }}</span>
                  <span class="flex items-center gap-1 truncate font-mono text-2xs font-normal text-muted"><Icon :name="Folder" :size="12" />{{ item }}</span>
                </span>
              </label>
            </div>
          </section>

          <UiAlert v-if="error">
            {{ error }}
          </UiAlert>
        </div>
      </fieldset>
      <footer class="modal-actions sticky bottom-0 flex items-center justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <p v-if="busy" class="m-0! mr-auto flex items-center gap-2 text-xs text-muted" role="status">
          <Icon :name="LoaderCircle" :size="14" class="animate-spin" />{{ busy }}
        </p>
        <UiButton :disabled="!!busy" @click="emit('close')">
          Cancel
        </UiButton>
        <UiButton variant="primary" type="submit" :disabled="!ready || !!busy || (candidates.length > 0 && !candidate)">
          {{ candidates.length ? 'Import selected skill' : 'Import skill' }}
        </UiButton>
      </footer>
    </form>
  </Modal>
</template>
