<script setup lang="ts">
import type { Skill } from '../../shared/contracts'
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
import Markdown from '../components/Markdown.vue'
import Modal from '../components/Modal.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import UiSegments from '../components/UiSegments.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  BookOpen,
  Code,
  Eye,
  FileCode,
  FolderGit2,
  Globe,
  Layers,
  Pencil,
  Plus,
  Search,
  Trash2,
} from '../icons'
import { iconButton } from '../ui'

const query = ref('')
const scopeFilter = ref('all')
const open = ref(false)
const preview = ref(false)
const original = ref<Skill>()
const name = ref('')
const scope = ref('global')
const content = ref('')
const error = ref('')
const busy = ref(false)
const deleting = ref<Skill>()
const files = ref<string[]>([])
const file = ref('SKILL.md')
const fileContent = ref('')
const newFile = ref('')
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
const fileOptions = computed(() => [...new Set([...files.value, file.value])].map(entry => ({ value: entry, label: entry, icon: FileCode })))
const items = computed(() =>
  state.skills.filter(
    s =>
      (scopeFilter.value === 'all' || s.scope === scopeFilter.value)
      && `${s.name} ${s.description}`
        .toLowerCase()
        .includes(query.value.toLowerCase()),
  ),
)

async function edit(skill?: Skill) {
  original.value = skill
  name.value = skill?.name ?? ''
  scope.value = skill?.scope ?? 'global'
  content.value
    = skill?.content
      ?? '---\nname: my-skill\ndescription: Describe when this skill should be used.\n---\n\n# My skill\n\nDescribe the workflow and how to verify completion.\n'
  file.value = 'SKILL.md'
  error.value = ''
  preview.value = false
  open.value = true
  files.value = skill
    ? await api(`/skills/${skill.scope}/${skill.name}/files`)
    : []
}

async function save() {
  busy.value = true
  error.value = ''
  try {
    if (file.value === 'SKILL.md') {
      await api(`/skills/${scope.value}/${name.value}`, {
        method: 'PUT',
        body: JSON.stringify({ content: content.value }),
      })
    }
    else {
      await api(`/skills/${scope.value}/${name.value}/file`, {
        method: 'PUT',
        body: JSON.stringify({ path: file.value, content: fileContent.value }),
      })
    }

    await refresh()
    notify('Skill saved')
    open.value = false
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function readFile(value: string) {
  error.value = ''
  try {
    if (value !== 'SKILL.md') {
      fileContent.value = (
        await api<{ content: string }>(
          `/skills/${scope.value}/${name.value}/file?path=${encodeURIComponent(value)}`,
        )
      ).content
    }

    file.value = value
  }
  catch (e) {
    error.value = (e as Error).message
  }
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
  <div class="page-heading flex items-center justify-between gap-5 mb-[27px] phone:gap-2.5 phone:flex-wrap phone:mb-[21px]">
    <div>
      <h1>Skills library</h1>
    </div>
    <UiButton variant="primary" @click="edit()">
      <Icon :name="Plus" :size="17" />New skill
    </UiButton>
  </div>
  <div class="toolbar flex items-center justify-between gap-5 mb-[23px] tablet:items-start tablet:flex-wrap phone:gap-4 phone:min-w-0">
    <div class="inline-label flex flex-row items-center gap-2.5 text-xs text-muted whitespace-nowrap min-w-0 max-w-full">
      <span>Scope</span><VirtualSelect
        v-model="scopeFilter"
        label="Scope"
        :options="scopeFilters"
        compact
        hide-label
      />
    </div><label class="search-field flex flex-row items-center gap-[7px] text-subtle bg-raised border border-line rounded-[7px] min-w-0 phone:w-full px-2.5 py-0"><Icon :name="Search" :size="17" /><input
      v-model="query"
      placeholder="Search skills"
      aria-label="Search skills"
    ></label>
  </div>
  <UiAlert v-if="error && !open">
    {{ error }}
  </UiAlert>
  <div v-if="items.length" class="skill-grid grid grid-cols-3 gap-5 compact:grid-cols-2 phone:grid-cols-1">
    <article
      v-for="skill in items"
      :key="`${skill.scope}/${skill.name}`"
      class="skill-card border-t border-line py-6"
    >
      <div class="skill-card-top flex items-center gap-2 mb-[17px]">
        <span class="skill-symbol text-muted bg-surface w-[35px] h-[35px] grid place-items-center rounded-[9px]"><Icon :name="BookOpen" :size="21" /></span><span class="pill inline-flex bg-soft text-accent rounded-[5px] text-xs font-[650] whitespace-nowrap px-2 py-1" :class="{ 'error-pill': !skill.valid }">{{
          !skill.valid
            ? "Needs attention"
            : skill.scope === "global"
              ? "Global"
              : state.projects.find((p) => p.id === skill.scope)?.name
        }}</span><button
          :class="twMerge(iconButton, 'icon-button')"
          :aria-label="`Edit ${skill.name}`"
          @click="edit(skill)"
        >
          <Icon :name="Pencil" :size="16" />
        </button>
      </div>
      <button class="skill-title font-heading [font-size:15px] font-[650] text-left wrap-anywhere p-0" @click="edit(skill)">
        {{ skill.name }}
      </button>
      <p>{{ skill.description }}</p>
      <footer>
        <code>.agents/skills</code><button
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
    title="Your playbook starts here"
    description="Add repeatable instructions for reviews, releases, research, and the work you do often."
  >
    <UiButton @click="edit()">
      <Icon :name="Plus" :size="16" />Create your first skill
    </UiButton>
  </Empty><Modal
    v-if="open"
    :title="original ? 'Edit skill' : 'Add to your playbook'"
    wide
    @close="open = false"
  >
    <form @submit.prevent="save">
      <div class="modal-body px-6.5 py-6 phone:p-5">
        <div class="form-grid grid grid-cols-[1fr_1fr] gap-5 phone:grid-cols-1 phone:gap-4.5">
          <label>Name<input
            v-model="name"
            aria-label="Skill name"
            :disabled="!!original"
            required
            pattern="[a-z0-9][a-z0-9\-]{0,63}"
            placeholder="my-skill"
          ><small>Match the name in your YAML frontmatter.</small></label><VirtualSelect
            v-model="scope"
            label="Scope"
            :options="scopes"
            :disabled="!!original"
          />
        </div>
        <div class="editor-toolbar flex justify-between items-center mt-4.5 mb-2.5 text-xs text-muted phone:flex-wrap phone:gap-2.5">
          <VirtualSelect
            v-if="original"
            :model-value="file"
            label="Skill file"
            :options="fileOptions"
            compact
            hide-label
            @update:model-value="readFile"
          /><span v-else>SKILL.md</span>
          <UiSegments
            v-model="preview"
            label="Skill editor view"
            compact
            :options="[{ value: false, label: 'Write', icon: Code }, { value: true, label: 'Preview', icon: Eye }]"
          />
        </div>
        <div v-if="preview" class="skill-preview h-82.5 overflow-auto border border-line rounded-lg p-5">
          <Markdown :content="file === 'SKILL.md' ? content : fileContent" />
        </div>
        <textarea
          v-else-if="file === 'SKILL.md'"
          v-model="content"
          class="code-editor font-mono! leading-[1.8]! text-xs! [tab-size:2]"
          rows="15"
          spellcheck="false"
          aria-label="Skill content"
        /><textarea
          v-else
          v-model="fileContent"
          class="code-editor font-mono! leading-[1.8]! text-xs! [tab-size:2]"
          rows="15"
          spellcheck="false"
          aria-label="Supporting file content"
        />
        <div v-if="original" class="supporting-file flex gap-2.5 mt-3.5 phone:flex-wrap">
          <input
            v-model="newFile"
            placeholder="Add supporting file, e.g. checklist.md"
            aria-label="New supporting file"
          ><UiButton
            type="button"
            size="small"
            :disabled="!newFile"
            @click="
              file = newFile;
              fileContent = '';
              newFile = '';
              preview = false;
            "
          >
            Add file
          </UiButton>
        </div>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton type="button" @click="open = false">
          Cancel
        </UiButton><UiButton variant="primary" type="submit" :disabled="busy">
          {{ busy ? "Saving…" : "Save skill" }}
        </UiButton>
      </footer>
    </form>
  </Modal><Modal
    v-if="deleting"
    title="Remove this skill?"
    @close="deleting = undefined"
  >
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <p>
        This removes “{{ deleting.name }}” and its supporting files from disk.
        Existing run snapshots remain available.
      </p>
    </div>
    <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
      <UiButton @click="deleting = undefined">
        Cancel
      </UiButton><UiButton variant="danger" @click="remove">
        Remove skill
      </UiButton>
    </footer>
  </Modal>
</template>
