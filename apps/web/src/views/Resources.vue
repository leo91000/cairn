<script setup lang="ts">
import type {
  Agent,
  PluginProject,
  Project,
} from '../../../../packages/contracts/contracts'
import type { ProjectSettings } from '../../../../packages/contracts/generated/typescript/ProjectSettings'
import type { Catalog } from '../plugins'
import {
  computed,
  nextTick,
  onMounted,
  ref,
} from 'vue'
import { providers as codingAgents } from '../../../../packages/contracts/accounts'
import { MAIN_AGENT_ID } from '../../../../packages/contracts/constants'
import { createProject } from '../../../../packages/contracts/generated/createProject'
import { deleteAgent } from '../../../../packages/contracts/generated/deleteAgent'
import { deleteProject } from '../../../../packages/contracts/generated/deleteProject'
import { listPluginPackages } from '../../../../packages/contracts/generated/listPluginPackages'
import { replaceProject } from '../../../../packages/contracts/generated/replaceProject'
import { accessSummary } from '../agent-access'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Empty from '../components/Empty.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import PluginProjectPicker from '../components/PluginProjectPicker.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  FolderGit2,
  GitBranch,
  MessageCircle,
  Pencil,
  Plus,
  ShieldCheck,
  Trash2,
} from '../icons'
import { pluginSources } from '../plugin-sources'
import { instanceNoun } from '../plugins'
import { useResourceSave } from '../resourceSave'
import {
  buttonBase,
  buttonSizes,
  buttonVariants,
  iconButton,
} from '../ui'

// Agents open their own page; projects are edited in a dialog.
const props = defineProps<{
  kind: 'agents' | 'projects'
}>()
const isAgent = computed(() => props.kind === 'agents')
const items = computed(() => [...state[props.kind]].sort((a, b) => Number(b.id === MAIN_AGENT_ID) - Number(a.id === MAIN_AGENT_ID)))
const taskCounts = computed(() => {
  const counts = new Map<string, number>()
  for (const task of state.tasks) {
    if (task.projectId && !task.archived)
      counts.set(task.projectId, (counts.get(task.projectId) || 0) + 1)
  }

  return counts
})

function repositoryLabel(origin?: string) {
  if (!origin)
    return 'Local project'
  try {
    const url = new URL(origin.replace(/^git@([^:]+):/, 'ssh://git@$1/'))
    return (url.hostname + url.pathname).replace(/\.git$/, '')
  }
  catch {
    return 'Git repository'
  }
}

const resourceSave = useResourceSave()
const editing = resourceSave.savedId
const pendingCreation = resourceSave.pendingCreation
let openedId: string | null = null
const saveAlert = ref<InstanceType<typeof UiAlert>>()
const open = ref(false)
const revision = ref('')
const deleting = ref<Agent | Project | null>(null)
const busy = ref(false)
const error = ref('')
const form = ref<ProjectSettings>({
  name: '',
  description: '',
  path: '',
  baseBranch: 'main',
  sourceMode: 'remote',
})
// "local", or the key of a plugin's project source to import from.
const projectMode = ref('local')
const instance = ref('')
const picked = ref('')

// Plugin names for the agents' access summary and project sources; members do not see plugins.
const catalog = ref<Catalog>({ packages: [], capabilities: [], connections: [] })
const sources = computed(() => pluginSources(catalog.value, 'projectSources'))
const importSource = computed(() => editing.value ? undefined : sources.value.find(source => source.key === projectMode.value))
const modes = computed(() => [
  { value: 'local', label: 'Server directory' },
  ...sources.value.map(source => ({ value: source.key, label: source.title, description: source.pluginName })),
])
// An instance is chosen only when the package has several.
const instances = computed(() => (importSource.value?.instances ?? []).map(connection => ({ value: connection.id, label: connection.name, description: connection.status.message || undefined })))
const instanceLabel = computed(() => instanceNoun(catalog.value.packages.find(entry => entry.id === importSource.value?.pluginId)?.manifest))

async function loadCatalog() {
  if (state.installationRole === 'owner')
    catalog.value = await listPluginPackages(api, {}).catch(() => catalog.value)
}

onMounted(loadCatalog)

function chooseSource() {
  instance.value = importSource.value?.instances[0]?.id ?? ''
  picked.value = ''
}

function selectProject(project: PluginProject) {
  picked.value = project.id
  form.value.name = (project.projectName || project.name).slice(0, 100)
  form.value.description = project.description.slice(0, 500)
  form.value.baseBranch = project.defaultBranch || form.value.baseBranch
}

const primaryLink = [buttonBase, buttonVariants.primary, buttonSizes.default].join(' ')

function edit(item?: Project) {
  if (state.installationRole === 'member')
    return

  projectMode.value = 'local'
  instance.value = ''
  picked.value = ''
  // Instances may have been added or paused since the page opened.
  if (!item)
    void loadCatalog()
  revision.value = item?.revision ?? ''
  openedId = item?.id ?? null
  resourceSave.reset(openedId)
  form.value = item
    ? {
        name: item.name,
        path: item.path,
        description: item.description,
        baseBranch: item.baseBranch,
        sourceMode: item.sourceMode,
      }
    : {
        name: '',
        description: '',
        path: '',
        baseBranch: 'main',
        sourceMode: 'remote',
      }
  form.value.sourceMode ??= 'remote'
  error.value = ''
  open.value = true
}

async function projectRequest<T>(path: string, options: RequestInit): Promise<T> {
  if (path === '/projects/import')
    return api<T>(path, options)
  const payload = JSON.parse(options.body as string)
  const saved = options.method === 'PUT'
    ? await replaceProject(api, { projectId: editing.value!, revision: revision.value, settings: form.value })
    : await createProject(api, {
        creationId: payload.creationId,
        settings: {
          name: payload.name,
          path: payload.path,
          description: payload.description,
          baseBranch: payload.baseBranch,
          sourceMode: payload.sourceMode,
        },
      })
  revision.value = saved.revision
  return saved as T
}

async function save() {
  if (busy.value || (importSource.value && !picked.value))
    return
  busy.value = true
  error.value = ''
  let phase: 'save' | 'refresh' = 'save'
  try {
    const importing = importSource.value
    await resourceSave.save<Project>(`/${props.kind}`, {
      body: () => form.value,
      request: projectRequest,
      createPath: importing ? '/projects/import' : undefined,
      createBody: importing
        ? () => ({
            pluginId: importing.pluginId,
            source: importing.source,
            instance: instance.value,
            id: picked.value,
            name: form.value.name,
            description: form.value.description,
            baseBranch: form.value.baseBranch,
          })
        : undefined,
      onCreated: (created) => {
        revision.value = created.revision
        if (importing) {
          form.value.path = created.path
          form.value.sourceMode = created.sourceMode
        }
      },
    })
    phase = 'refresh'
    await refresh()
    open.value = false
    notify('Project saved')
  }
  catch (e) {
    const detail = (e as Error).message
    let summary: string
    if (phase === 'refresh')
      summary = 'Project saved, but the workspace could not be reloaded.'
    else if (editing.value && editing.value !== openedId)
      summary = 'Project saved, but the latest changes could not be saved.'
    else if (pendingCreation.value)
      summary = 'Could not confirm whether the project was saved. Retry to recover it safely.'
    else
      summary = 'Could not save the project.'
    error.value = detail ? `${summary} ${detail}` : summary

    // The alert follows the whole form; bring it above the sticky actions on small screens.
    await nextTick()
    saveAlert.value?.$el.scrollIntoView?.({ block: 'nearest' })
  }
  finally {
    busy.value = false
  }
}

async function remove() {
  if (!deleting.value)
    return

  try {
    if (isAgent.value)
      await deleteAgent(api, { agentId: deleting.value.id })
    else
      await deleteProject(api, { projectId: deleting.value.id })
    await refresh()
    deleting.value = null
    notify('Removed')
  }
  catch (e) {
    error.value = (e as Error).message
  }
}
</script>

<template>
  <div class="page-heading flex items-center justify-between gap-5 mb-[27px] phone:gap-2.5 phone:flex-wrap phone:mb-[21px]">
    <div>
      <h1>{{ isAgent ? "Agents" : "Projects" }}</h1>
    </div>
    <template v-if="state.installationRole === 'owner'">
      <RouterLink v-if="isAgent" to="/agents/new" :class="primaryLink">
        <Icon :name="Plus" :size="17" />New agent
      </RouterLink>
      <UiButton v-else variant="primary" @click="edit()">
        <Icon :name="Plus" :size="17" />Add project
      </UiButton>
    </template>
  </div>
  <UiAlert v-if="error && !open">
    {{ error }}
  </UiAlert>
  <div v-if="items.length" class="resource-grid grid" :class="isAgent ? 'grid-cols-2 gap-4 tablet:grid-cols-1' : 'grid-cols-1 border-t border-line'">
    <article
      v-for="item in items"
      :key="item.id"
      class="resource-card grid min-w-0 gap-x-3"
      :class="isAgent ? 'grid-cols-[36px_minmax(0,1fr)] grid-rows-[1fr_auto] gap-y-4 rounded-xl border border-line/70 bg-surface/50 p-5 phone:p-4' : 'grid-cols-[36px_minmax(0,1fr)_auto] items-center border-b border-line py-4 phone:items-start phone:gap-y-2'"
    >
      <AgentAvatar v-if="isAgent" :name="item.name" :identity="item.id" />
      <span v-else class="grid size-9 place-items-center rounded-lg text-accent bg-soft"><Icon :name="FolderGit2" :size="20" /></span>
      <div class="min-w-0" :class="!isAgent ? 'flex items-center gap-5 phone:block' : ''">
        <div class="min-w-0 flex-1">
          <h2 class="text-sm font-semibold">
            <RouterLink
              v-if="state.installationRole === 'owner' && isAgent"
              :to="`/agents/${item.id}`"
              class="block max-w-full truncate text-left hover:text-accent focus-visible:rounded"
              :title="item.name"
            >
              {{ item.name }}
            </RouterLink>
            <button
              v-else-if="state.installationRole === 'owner'"
              class="max-w-full truncate text-left hover:text-accent focus-visible:rounded"
              :title="item.name"
              @click="edit(item as Project)"
            >
              {{ item.name }}
            </button>
            <span v-else>{{ item.name }}</span>
          </h2>
          <p v-if="isAgent && item.description" class="mt-1 line-clamp-2 text-xs text-muted" :title="item.description">
            {{ item.description }}
          </p>
          <p v-if="'path' in item" class="mt-0.5 truncate text-xs text-muted" :title="repositoryLabel(item.origin)">
            {{ repositoryLabel(item.origin) }}
          </p>
        </div>
        <div v-if="'reasoning' in item" class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted">
          <span class="max-w-full truncate" :title="item.model || 'Provider default'">{{ `${codingAgents[item.provider || 'codex'].label} · ${item.model || 'Provider default'}` }}</span>
          <span class="inline-flex min-w-0 items-center gap-1.5"><Icon :name="ShieldCheck" :size="13" class="shrink-0" /><span class="min-w-0">{{ accessSummary(item, catalog) }}</span></span>
        </div>
        <div v-else-if="'path' in item" class="flex shrink-0 items-center gap-5 pr-6 text-xs text-muted phone:mt-2 phone:gap-4 phone:pr-0">
          <span class="inline-flex max-w-36 items-center gap-1.5" :title="item.baseBranch"><Icon :name="GitBranch" :size="13" /><span class="truncate">{{ item.baseBranch }}</span></span>
          <span class="min-w-12 tabular-nums">{{ taskCounts.get(item.id) || 0 }} {{ taskCounts.get(item.id) === 1 ? 'task' : 'tasks' }}</span>
        </div>
      </div>
      <div class="flex items-center justify-between gap-4" :class="isAgent ? 'col-span-2 border-t border-line/60 pt-3' : 'col-start-3 row-start-1 phone:col-start-2 phone:row-start-2'">
        <RouterLink :to="{ path: '/chats', query: { [isAgent ? 'agent' : 'project']: item.id } }" class="inline-flex min-h-8 items-center gap-2 rounded-lg text-xs font-medium text-accent hover:underline focus-visible:outline-2 focus-visible:outline-accent phone:min-h-11">
          <Icon :name="MessageCircle" :size="15" />Start chat
        </RouterLink>
        <div v-if="state.installationRole === 'owner'" class="flex items-center gap-1">
          <RouterLink
            v-if="isAgent"
            :to="`/agents/${item.id}`"
            :class="iconButton"
            :aria-label="`Edit ${item.name}`"
            :title="`Edit ${item.name}`"
          >
            <Icon :name="Pencil" :size="15" />
          </RouterLink>
          <button
            v-else
            :class="iconButton"
            :aria-label="`Edit ${item.name}`"
            :title="`Edit ${item.name}`"
            @click="edit(item as Project)"
          >
            <Icon :name="Pencil" :size="15" />
          </button>
          <button
            v-if="item.id !== MAIN_AGENT_ID"
            :class="iconButton"
            :aria-label="`Delete ${item.name}`"
            :title="`Delete ${item.name}`"
            @click="deleting = item"
          >
            <Icon :name="Trash2" :size="15" />
          </button>
        </div>
      </div>
    </article>
  </div>
  <Empty
    v-else
    :title="isAgent ? 'Meet your first agent' : 'Where should the work happen?'"
    :description="
      isAgent
        ? 'Create a profile with the right instructions and model for the job.'
        : 'Choose an existing directory on your server, or import a project through a plugin such as GitHub. Agents work on private copies in their workspaces.'
    "
  >
    <template v-if="state.installationRole === 'owner'">
      <RouterLink v-if="isAgent" to="/agents/new" :class="primaryLink">
        <Icon :name="Plus" :size="16" />Create an agent
      </RouterLink>
      <UiButton v-else @click="edit()">
        <Icon :name="Plus" :size="16" />Add a project
      </UiButton>
    </template>
  </Empty><Modal
    v-if="open"
    :title="`${editing ? 'Edit' : 'New'} project`"
    @close="!busy && (open = false)"
  >
    <form @submit.prevent="save">
      <div class="modal-body form-grid grid grid-cols-[1fr_1fr] gap-5 phone:grid-cols-1 phone:gap-4.5 px-6.5 py-6 phone:p-5">
        <div v-if="!editing" class="col-span-2 phone:col-span-1 grid gap-4">
          <VirtualSelect
            v-model="projectMode"
            label="Add from"
            :disabled="busy || pendingCreation"
            :options="modes"
            @update:model-value="chooseSource"
          />
          <VirtualSelect
            v-if="importSource && instances.length > 1"
            v-model="instance"
            :label="instanceLabel"
            :disabled="busy || pendingCreation"
            :options="instances"
            @update:model-value="picked = ''"
          />
          <PluginProjectPicker
            v-if="importSource && instance"
            :source="importSource"
            :instance="instance"
            :disabled="busy || pendingCreation"
            :selected="picked"
            @select="selectProject"
          />
        </div>
        <label class="span-2 col-span-2 phone:col-span-1">Name<input
          v-model="form.name"
          required
          autofocus
          maxlength="100"
          placeholder="e.g. SheetOM"
        ></label><label class="span-2 col-span-2 phone:col-span-1">Description<textarea
          v-model="form.description"
          rows="2"
          maxlength="500"
          placeholder="A short reminder of what this is for."
        /></label>
        <VirtualSelect
          v-if="!importSource"
          v-model="form.sourceMode"
          label="New work starts from"
          :options="[{ value: 'remote', label: 'Latest remote branch', description: 'Fetch a fresh copy; local files stay untouched' }, { value: 'local', label: 'Local branch snapshot', description: 'Use committed files from the local branch' }]"
        />
        <label v-if="!importSource" class="span-2 col-span-2 phone:col-span-1">Project directory<input
          v-model="form.path"
          required
          placeholder="/workspaces/my-project"
        ><small>Must be inside one of your configured workspace roots.</small></label><label class="span-2 col-span-2 phone:col-span-1">Base branch<input
          v-model="form.baseBranch"
          required
          placeholder="main"
        ><small>Used for new private workspaces.</small></label>
        <UiAlert v-if="error" ref="saveAlert" class="col-span-2 scroll-mb-24 phone:col-span-1">
          {{ error }}
        </UiAlert>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton type="button" :disabled="busy" @click="open = false">
          Cancel
        </UiButton><UiButton variant="primary" type="submit" :disabled="busy || (importSource && !picked)">
          {{ busy ? "Saving…" : "Save project" }}
        </UiButton>
      </footer>
    </form>
  </Modal><Modal v-if="deleting" title="Remove this item?" @close="deleting = null">
    <div class="modal-body px-6.5 py-6 phone:p-5">
      <p>
        Remove “{{ deleting.name }}” from your workspace? Referenced items must
        be removed from tasks first.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
    </div>
    <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
      <UiButton @click="deleting = null">
        Cancel
      </UiButton><UiButton variant="danger" @click="remove">
        Remove
      </UiButton>
    </footer>
  </Modal>
</template>
