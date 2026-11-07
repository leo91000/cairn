<script setup lang="ts">
import type { SkillBundle } from '../../../../packages/contracts/contracts'
import type { ImportSkillInput } from '../../../../packages/contracts/generated/typescript/ImportSkillInput'
import type { Catalog } from '../plugins'
import {
  computed,
  onMounted,
  ref,
  watch,
} from 'vue'
import { importSkill } from '../../../../packages/contracts/generated/importSkill'
import { listPluginPackages } from '../../../../packages/contracts/generated/listPluginPackages'
import { api, ApiError, state } from '../api'
import {
  BookOpen,
  Check,
  FileArchive,
  Folder,
  FolderGit2,
  Globe,
  LoaderCircle,
  Puzzle,
  Upload,
} from '../icons'
import { pluginFormValues, pluginSettings } from '../plugin-form-values'
import { pluginRequest } from '../plugin-operations'
import { pluginSources } from '../plugin-sources'
import { instanceNoun } from '../plugins'
import { defaultSkillLimits, formatBytes } from '../skill-bundle'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import PluginFields from './PluginFields.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'
import VirtualSelect from './VirtualSelect.vue'

// Imports a skill from a .zip/.skill archive or a plugin's skill source, such as a GitHub
// repository. When the source holds several skills, the user picks one and the same upload or
// location is submitted again.
const emit = defineEmits<{ close: [], imported: [bundle: SkillBundle] }>()
// "archive", or the key of a plugin's skill source.
const source = ref('archive')
const scope = ref('global')
const archive = ref<File>()
const upload = ref<{ id: string, file: File }>()
const catalog = ref<Catalog>({ packages: [], capabilities: [], connections: [] })
const instance = ref('')
const location = ref<Record<string, any>>({})
const folder = ref('')
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
const sources = computed(() => pluginSources(catalog.value, 'skillSources'))
const plugin = computed(() => sources.value.find(item => item.key === source.value))
const choices = computed(() => [
  { value: 'archive', label: 'Archive', icon: FileArchive },
  ...sources.value.map(item => ({ value: item.key, label: item.title, icon: Puzzle })),
])
// An instance is chosen only when the package has several; otherwise its only one is used.
const instances = computed(() => (plugin.value?.instances ?? []).map(connection => ({ value: connection.id, label: connection.name })))
const instanceLabel = computed(() => instanceNoun(catalog.value.packages.find(entry => entry.id === plugin.value?.pluginId)?.manifest))
const missing = computed(() => (plugin.value?.fields ?? []).some(field => field.required && !field.hidden && !String(location.value[field.key] ?? '').trim()))
const ready = computed(() => source.value === 'archive' ? !!archive.value : !!plugin.value && !missing.value)

onMounted(async () => {
  // Members do not see plugins: they import archives.
  if (state.installationRole === 'owner')
    catalog.value = await listPluginPackages(api, {}).catch(() => catalog.value)
})

watch(source, () => {
  instance.value = plugin.value?.instances[0]?.id ?? ''
  location.value = pluginFormValues(plugin.value?.fields ?? [])
  folder.value = ''
})
watch([source, scope, instance, location, folder], () => {
  candidates.value = []
  candidate.value = ''
  error.value = ''
}, { deep: true })

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

async function submit() {
  if (!ready.value || busy.value)
    return
  error.value = ''
  const kind = source.value
  const chosen = plugin.value
  const destination = scope.value
  const submittedArchive = archive.value
  // A candidate is a folder the source holds; the plugin parses its own locations.
  const requestedFolder = candidate.value || (chosen ? folder.value.trim().replace(/^\/+|\/+$/g, '') : '')
  busy.value = chosen ? `Fetching from ${chosen.pluginName}…` : 'Uploading archive…'
  try {
    let body: ImportSkillInput
    if (!chosen) {
      // Uploads are consumed only by a successful import, so a candidate pick reuses this one.
      if (!upload.value || upload.value.file !== submittedArchive) {
        busy.value = 'Uploading archive…'
        const staged = await api<{ id: string }>('/skills/uploads', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: submittedArchive })
        upload.value = { id: staged.id, file: submittedArchive! }
      }

      body = {
        source: 'archive',
        scope: destination,
        upload: upload.value.id,
        ...(requestedFolder ? { path: requestedFolder } : {}),
      }
    }
    else {
      body = {
        source: 'plugin',
        scope: destination,
        pluginId: chosen.pluginId,
        skillSource: chosen.source,
        ...(instance.value ? { instance: instance.value } : {}),
        location: pluginSettings(chosen.fields, location.value),
        ...(requestedFolder ? { folder: requestedFolder } : {}),
      }
    }

    busy.value = chosen ? `Fetching from ${chosen.pluginName}…` : 'Importing…'
    const result = await importSkill(pluginRequest, body)
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
    if (e instanceof ApiError && e.status === 404 && kind === 'archive')
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
            :options="choices"
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

          <template v-else-if="plugin">
            <p v-if="plugin.description" class="m-0! text-xs text-muted">
              {{ plugin.description }}
            </p>
            <VirtualSelect
              v-if="instances.length > 1"
              v-model="instance"
              :label="instanceLabel"
              :options="instances"
              :disabled="!!busy"
            />
            <PluginFields
              v-model:values="location"
              :fields="plugin.fields"
              :disabled="!!busy"
            />
            <label>Folder <span class="font-normal text-subtle">(optional)</span><input
              v-model="folder"
              aria-label="Folder"
              placeholder="skills/review"
              autocomplete="off"
              spellcheck="false"
            ><small>Where the skill is within this location, when it holds several.</small></label>
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
