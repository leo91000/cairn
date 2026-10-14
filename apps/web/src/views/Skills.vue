<script setup lang="ts">
import type { Skill, SkillBundle } from '../../../../packages/contracts/contracts'
import { twMerge } from 'tailwind-merge'
import { computed, ref } from 'vue'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import Empty from '../components/Empty.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import SkillImport from '../components/SkillImport.vue'
import SkillWorkbench from '../components/SkillWorkbench.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  BookOpen,
  Files,
  FolderGit2,
  Github,
  Globe,
  HardDrive,
  Import,
  Layers,
  Pencil,
  Plus,
  Search,
  Terminal,
  Trash2,
} from '../icons'
import { formatBytes, plural, shortCommit } from '../skill-bundle'
import { iconButton } from '../ui'

const query = ref('')
const scopeFilter = ref('all')
const error = ref('')
const deleting = ref<Skill>()
const importing = ref(false)
// `undefined` keeps the workbench closed; `null` opens it on a new skill.
const editing = ref<{ scope: string, name: string } | null>()
const owner = computed(() => state.installationRole === 'owner')
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
const scopeFilters = computed(() => [{ value: 'all', label: 'All skills', icon: Layers }, ...scopes.value])
const items = computed(() =>
  state.skills.filter(
    skill =>
      (scopeFilter.value === 'all' || skill.scope === scopeFilter.value)
      && `${skill.name} ${skill.description} ${skill.source?.repository ?? ''}`
        .toLowerCase()
        .includes(query.value.toLowerCase()),
  ),
)

function scopeName(skill: Skill) {
  return skill.scope === 'global' ? 'Global' : state.projects.find(project => project.id === skill.scope)?.name ?? 'Project'
}

function open(skill?: Skill) {
  if (!skill && !owner.value)
    return
  editing.value = skill ? { scope: skill.scope, name: skill.name } : null
}

async function imported(bundle: SkillBundle) {
  importing.value = false
  notify(`Imported ${bundle.name}`)
  editing.value = { scope: bundle.scope, name: bundle.name }
  await refresh()
}

async function remove() {
  try {
    await api(`/skills/${deleting.value!.scope}/${deleting.value!.name}`, {
      method: 'DELETE',
    })
    deleting.value = undefined
    await refresh()
    notify('Skill removed')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}
</script>

<template>
  <div class="page-heading mb-[27px] flex items-center justify-between gap-5 phone:mb-[21px] phone:flex-wrap phone:gap-2.5">
    <div>
      <h1>Skills library</h1>
    </div>
    <div v-if="owner" class="flex items-center gap-2.5 phone:w-full">
      <UiButton class="phone:flex-1" @click="importing = true">
        <Icon :name="Import" :size="17" />Import
      </UiButton>
      <UiButton variant="primary" class="phone:flex-1" @click="open()">
        <Icon :name="Plus" :size="17" />New skill
      </UiButton>
    </div>
  </div>
  <div class="toolbar mb-[23px] flex items-center justify-between gap-5 tablet:flex-wrap tablet:items-start phone:min-w-0 phone:gap-4">
    <div class="inline-label flex max-w-full min-w-0 flex-row items-center gap-2.5 text-xs whitespace-nowrap text-muted">
      <span>Scope</span><VirtualSelect
        v-model="scopeFilter"
        label="Scope"
        :options="scopeFilters"
        compact
        hide-label
      />
    </div><label class="search-field flex min-w-0 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle phone:w-full"><Icon :name="Search" :size="17" /><input
      v-model="query"
      placeholder="Search skills"
      aria-label="Search skills"
    ></label>
  </div>
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <div v-if="items.length" class="skill-grid grid grid-cols-3 gap-x-5 gap-y-2 compact:grid-cols-2 phone:grid-cols-1!">
    <article
      v-for="skill in items"
      :key="`${skill.scope}/${skill.name}`"
      class="skill-card group flex min-w-0 flex-col border-t border-line py-6"
    >
      <div class="skill-card-top mb-[17px] flex min-w-0 items-center gap-2">
        <span class="skill-symbol grid h-[35px] w-[35px] shrink-0 place-items-center rounded-[9px] bg-surface text-muted transition-colors group-hover:bg-soft group-hover:text-accent"><Icon :name="BookOpen" :size="21" /></span>
        <span class="pill inline-flex shrink-0 rounded-[5px] bg-soft px-2 py-1 text-xs font-[650] whitespace-nowrap text-accent" :class="{ 'error-pill': !skill.valid }">{{ skill.valid ? scopeName(skill) : 'Needs attention' }}</span>
        <span
          v-if="skill.source"
          class="inline-flex min-w-0 items-center gap-1.5 rounded-[5px] border border-line bg-surface px-1.5 py-[3px] text-2xs text-muted"
          :title="`Imported from github.com/${skill.source.repository}${skill.source.path ? `/${skill.source.path}` : ''} at ${shortCommit(skill.source.commit)}${skill.source.modified ? ' · edited since' : ''}`"
          data-testid="skill-source"
        >
          <Icon :name="Github" :size="12" class="shrink-0 text-ink" />
          <span class="truncate font-medium text-ink">{{ skill.source.repository }}</span>
          <code class="shrink-0 text-3xs">{{ shortCommit(skill.source.commit) }}</code>
          <span v-if="skill.source.modified" class="shrink-0 text-3xs font-semibold text-warning">· edited</span>
        </span>
        <button
          v-if="owner"
          :class="twMerge(iconButton, 'icon-button')"
          :aria-label="`Edit ${skill.name}`"
          @click="open(skill)"
        >
          <Icon :name="Pencil" :size="16" />
        </button>
      </div>
      <button class="skill-title p-0 text-left font-heading [font-size:15px] font-[650] wrap-anywhere hover:underline" @click="open(skill)">
        {{ skill.name }}
      </button>
      <p class="flex-1">
        {{ skill.valid ? skill.description : skill.error || skill.description }}
      </p>
      <footer class="gap-3">
        <span class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-2xs tabular-nums">
          <span class="inline-flex items-center gap-1" :title="plural(skill.files ?? 1, 'file')"><Icon :name="Files" :size="13" />{{ plural(skill.files ?? 1, 'file') }}</span>
          <span v-if="skill.bytes !== undefined" class="inline-flex items-center gap-1"><Icon :name="HardDrive" :size="13" />{{ formatBytes(skill.bytes) }}</span>
          <span v-if="skill.executables" class="inline-flex items-center gap-1 text-success"><Icon :name="Terminal" :size="13" />{{ plural(skill.executables, 'script') }}</span>
        </span><button
          v-if="owner"
          :class="twMerge(iconButton, 'icon-button')"
          :aria-label="`Delete ${skill.name}`"
          @click="deleting = skill"
        >
          <Icon :name="Trash2" :size="15" />
        </button>
      </footer>
    </article>
  </div>
  <Empty
    v-else
    :title="query || scopeFilter !== 'all' ? 'No matching skills' : 'Your playbook starts here'"
    :description="query || scopeFilter !== 'all' ? 'Try another search or scope.' : 'Add repeatable instructions, scripts and references for reviews, releases, research, and the work you do often.'"
  >
    <div v-if="owner && !query && scopeFilter === 'all'" class="flex flex-wrap justify-center gap-2.5">
      <UiButton @click="importing = true">
        <Icon :name="Import" :size="16" />Import a skill
      </UiButton>
      <UiButton variant="primary" @click="open()">
        <Icon :name="Plus" :size="16" />Create your first skill
      </UiButton>
    </div>
  </Empty>
  <SkillWorkbench
    v-if="editing !== undefined"
    :key="editing ? `${editing.scope}/${editing.name}` : 'new'"
    :target="editing"
    @close="editing = undefined"
  />
  <SkillImport v-if="importing" @close="importing = false" @imported="imported" />
  <Modal
    v-if="deleting"
    title="Remove this skill?"
    @close="deleting = undefined"
  >
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <p>
        This removes “{{ deleting.name }}” and its {{ plural(deleting.files ?? 1, 'file') }} from disk.
        Existing run snapshots remain available.
      </p>
    </div>
    <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
      <UiButton @click="deleting = undefined">
        Cancel
      </UiButton><UiButton variant="danger" @click="remove">
        Remove skill
      </UiButton>
    </footer>
  </Modal>
</template>
