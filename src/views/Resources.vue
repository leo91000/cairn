<script setup lang="ts">
import type { GithubRepository } from '../../shared/contracts'
import type { ExecutionNode } from '../../shared/nodes'
import { computed, ref } from 'vue'
import { MAIN_AGENT_ID } from '../../shared/constants'
import { LOCAL_NODE_ID } from '../../shared/nodes'
import { api, notify, refresh, state } from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import AgentPortraitEditor from '../components/AgentPortraitEditor.vue'
import AssistantPicker from '../components/AssistantPicker.vue'
import Empty from '../components/Empty.vue'
import GithubRepositoryPicker from '../components/GithubRepositoryPicker.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import { FolderGit2, GitBranch, MessageCircle, Pencil, Plus, ShieldCheck, Trash2 } from '../icons'
import { iconButton } from '../ui'

const props = defineProps<{
  kind: 'agents' | 'projects'
}>()
const nodes = ref<ExecutionNode[]>([])
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
const editing = ref<string | null>(null)
const open = ref(false)
const deleting = ref<any>(null)
const busy = ref(false)
const error = ref('')
const form = ref<any>({})
const allNodes = computed({
  get: () => form.value.access?.nodes === null,
  set: (value: boolean) => {
    form.value.access.nodes = value ? null : []
  },
})
const projectMode = ref('local')
const repository = ref('')
function selectRepository(repo: GithubRepository) {
  repository.value = repo.fullName
  form.value.name = repo.name.slice(0, 100)
  form.value.description = repo.description.slice(0, 500)
  form.value.baseBranch = repo.defaultBranch
}
const githubImport = computed(() => !isAgent.value && !editing.value && projectMode.value === 'github')
const githubToken = ref('')
const githubConfigured = ref(false)
const removeGithub = ref(false)
const sandboxOptions = [
  { value: 'yolo', label: 'YOLO', description: 'Autonomous execution · default' },
  { value: 'workspace-write', label: 'Workspace write', description: 'Limit writes to task workspaces' },
  { value: 'read-only', label: 'Read only', description: 'Project mounts are read-only; no approval escalation' },
]
const allProjects = computed({ get: () => form.value.access?.projects === null, set: (value: boolean) => {
  form.value.access.projects = value ? null : []
  if (!value)
    form.value.access.github = false
} })
const allMcps = computed({ get: () => form.value.access?.mcps === null, set: (value: boolean) => {
  form.value.access.mcps = value ? null : []
  form.value.access.mcpTools = {}
} })
function toggleMcp(id: string, enabled: boolean) {
  if (enabled) {
    form.value.access.mcps.push(id)
  }
  else {
    form.value.access.mcps = form.value.access.mcps.filter((value: string) => value !== id)
    delete form.value.access.mcpTools[id]
  }
}
const allSkills = computed({ get: () => form.value.access?.skills === null, set: (value: boolean) => {
  form.value.access.skills = value ? null : []
} })
const permittedSkills = computed(() => state.skills.filter(skill => skill.valid && (skill.scope === 'global' || form.value.access?.projects === null || form.value.access?.projects?.includes(skill.scope))))
async function edit(item?: any) {
  projectMode.value = 'local'
  repository.value = ''
  githubToken.value = ''
  githubConfigured.value = false
  removeGithub.value = false
  editing.value = item?.id ?? null
  form.value = item
    ? structuredClone(JSON.parse(JSON.stringify(item)))
    : isAgent.value
      ? {
          name: '',
          provider: 'codex',
          description: '',
          model: '',
          reasoning: '',
          instructions: '',
          timeoutMinutes: 0,
          access: { nodes: [LOCAL_NODE_ID], projects: null, skills: null, mcps: null, mcpTools: {}, github: true, sandbox: 'yolo' },
        }
      : { name: '', description: '', path: '', baseBranch: 'main', sourceMode: 'remote' }
  if (isAgent.value) {
    form.value.provider ??= 'codex'
    form.value.access.nodes ??= form.value.access.nodes === null ? null : [LOCAL_NODE_ID]
    try {
      nodes.value = await api<ExecutionNode[]>('/nodes')
    }
    catch (e) {
      error.value = (e as Error).message
      return
    }
  }
  if (!isAgent.value)
    form.value.sourceMode ??= 'remote'
  error.value = ''
  open.value = true
  if (isAgent.value && editing.value) {
    const id = editing.value
    try {
      const connection = await api(`/agents/${id}/github-token`)
      if (editing.value === id)
        githubConfigured.value = connection.configured
    }
    catch (e) {
      error.value = (e as Error).message
    }
  }
}
async function save() {
  if (busy.value || (githubImport.value && !repository.value))
    return
  busy.value = true
  error.value = ''
  try {
    const saved = await api(githubImport.value ? '/projects/github' : `/${props.kind}${editing.value ? `/${editing.value}` : ''}`, {
      method: editing.value ? 'PUT' : 'POST',
      body: JSON.stringify(githubImport.value ? { repository: repository.value, name: form.value.name, description: form.value.description, baseBranch: form.value.baseBranch } : form.value),
    })
    if (isAgent.value && (githubToken.value || removeGithub.value))
      await api(`/agents/${saved.id}/github-token`, { method: 'PUT', body: JSON.stringify({ token: removeGithub.value ? '' : githubToken.value }) })
    githubToken.value = ''
    await refresh()
    open.value = false
    notify(`${isAgent.value ? 'Agent' : 'Project'} saved`)
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}
async function remove() {
  try {
    await api(`/${props.kind}/${deleting.value.id}`, { method: 'DELETE' })
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
    <UiButton variant="primary" @click="edit()">
      <Icon :name="Plus" :size="17" />{{ isAgent ? "New agent" : "Add project" }}
    </UiButton>
  </div>
  <UiAlert v-if="error && !open">
    {{ error }}
  </UiAlert>
  <div v-if="items.length" class="resource-grid grid" :class="isAgent ? 'grid-cols-2 gap-4 tablet:grid-cols-1' : 'grid-cols-1 border-t border-line'">
    <article v-for="item in items" :key="item.id" class="resource-card grid min-w-0 gap-x-3" :class="isAgent ? 'grid-cols-[36px_minmax(0,1fr)] grid-rows-[1fr_auto] gap-y-4 rounded-xl border border-line/70 bg-surface/50 p-5 phone:p-4' : 'grid-cols-[36px_minmax(0,1fr)_auto] items-center border-b border-line py-4 phone:items-start phone:gap-y-2'">
      <AgentAvatar v-if="isAgent" :name="item.name" :identity="item.id" />
      <span v-else class="grid size-9 place-items-center rounded-lg text-accent bg-soft"><Icon :name="FolderGit2" :size="20" /></span>
      <div class="min-w-0" :class="!isAgent ? 'flex items-center gap-5 phone:block' : ''">
        <div class="min-w-0 flex-1">
          <h2 class="text-sm font-semibold">
            <button class="max-w-full truncate text-left hover:text-accent focus-visible:rounded" :title="item.name" @click="edit(item)">
              {{ item.name }}
            </button>
          </h2>
          <p v-if="isAgent && item.description" class="mt-1 line-clamp-2 text-xs text-muted" :title="item.description">
            {{ item.description }}
          </p>
          <p v-if="'path' in item" class="mt-0.5 truncate text-xs text-muted" :title="repositoryLabel(item.origin)">
            {{ repositoryLabel(item.origin) }}
          </p>
        </div>
        <div v-if="'reasoning' in item" class="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted">
          <span class="max-w-full truncate" :title="item.model || 'Provider default'">{{ (item.provider === 'claude' ? 'Claude Code · ' : 'Codex · ') + (item.model || 'Provider default') }}</span>
          <span class="inline-flex items-center gap-1.5"><Icon :name="ShieldCheck" :size="13" />{{ item.access.projects === null ? 'All projects' : item.access.projects.length === 0 ? 'No projects' : item.access.projects.length === 1 ? '1 project' : `${item.access.projects.length} projects` }}</span>
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
        <div class="flex items-center gap-1">
          <button :class="iconButton" :aria-label="`Edit ${item.name}`" :title="`Edit ${item.name}`" @click="edit(item)">
            <Icon :name="Pencil" :size="15" />
          </button>
          <button v-if="item.id !== MAIN_AGENT_ID" :class="iconButton" :aria-label="`Delete ${item.name}`" :title="`Delete ${item.name}`" @click="deleting = item">
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
        : 'Choose a GitHub repository or an existing directory on your server. Agents work on private copies in their VMs.'
    "
  >
    <UiButton @click="edit()">
      <Icon :name="Plus" :size="16" />{{ isAgent ? "Create an agent" : "Add a project" }}
    </UiButton>
  </Empty><Modal
    v-if="open"
    :title="`${editing ? 'Edit' : 'New'} ${isAgent ? 'agent' : 'project'}`"
    @close="!busy && (open = false)"
  >
    <form @submit.prevent="save">
      <div class="modal-body form-grid grid grid-cols-[1fr_1fr] gap-5 phone:grid-cols-1 phone:gap-4.5 px-6.5 py-6 phone:p-5">
        <AgentPortraitEditor v-if="isAgent" :key="editing || 'new'" :agent-id="editing" :name="form.name" :disabled="busy" />
        <div v-if="!isAgent && !editing" class="col-span-2 phone:col-span-1 grid gap-4">
          <VirtualSelect v-model="projectMode" label="Add from" :disabled="busy" :options="[{ value: 'local', label: 'Server directory' }, { value: 'github', label: 'GitHub' }]" />
          <GithubRepositoryPicker v-if="githubImport" :disabled="busy" :selected="repository" @select="selectRepository" />
        </div>
        <label class="span-2 col-span-2 phone:col-span-1">Name<input
          v-model="form.name"
          required
          autofocus
          maxlength="100"
          :placeholder="
            isAgent ? 'e.g. Release engineer' : 'e.g. SheetOM'
          "
        ></label><label class="span-2 col-span-2 phone:col-span-1">Description<textarea
          v-model="form.description"
          rows="2"
          maxlength="500"
          placeholder="A short reminder of what this is for."
        /></label><template v-if="isAgent">
          <AssistantPicker v-model:provider="form.provider" v-model:model="form.model" v-model:reasoning="form.reasoning" variant="field" :disabled="busy" /><div class="span-2 col-span-2 phone:col-span-1 agent-access-panel">
            <div class="agent-access-heading">
              <Icon :name="ShieldCheck" :size="20" /><div><h3>Access &amp; execution</h3><p>Choose the resources this agent can use.</p></div>
            </div>
            <template v-if="editing !== MAIN_AGENT_ID">
              <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="allProjects" type="checkbox">All projects, including future projects</label>
              <fieldset v-if="!allProjects" class="access-choices">
                <legend>Allowed projects</legend><label v-for="project in state.projects" :key="project.id" class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="form.access.projects" type="checkbox" :value="project.id">{{ project.name }}</label><small v-if="!state.projects.length">No projects registered yet.</small>
              </fieldset>
              <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="allSkills" type="checkbox">All available skills</label>
              <fieldset v-if="!allSkills" class="access-choices">
                <legend>Allowed skills</legend><label v-for="skill in permittedSkills" :key="skill.path" class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="form.access.skills" type="checkbox" :value="`${skill.scope}/${skill.name}`">{{ skill.name }}</label>
              </fieldset>
              <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="allMcps" type="checkbox">All MCP connections, including future connections</label>
              <fieldset class="access-choices">
                <legend>MCP permissions</legend>
                <div v-for="connection in state.mcps" :key="connection.id">
                  <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input type="checkbox" :checked="allMcps || form.access.mcps.includes(connection.id)" :disabled="allMcps" @change="toggleMcp(connection.id, ($event.target as HTMLInputElement).checked)">{{ connection.name }}</label>
                  <details v-if="allMcps || form.access.mcps.includes(connection.id)" class="mcp-agent-tools mt-[7px] mr-0 mb-4 ml-6 text-xs">
                    <summary>Tool access</summary>
                    <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input type="checkbox" :checked="!(connection.id in form.access.mcpTools)" @change="($event.target as HTMLInputElement).checked ? delete form.access.mcpTools[connection.id] : form.access.mcpTools[connection.id] = []">All enabled tools</label>
                    <template v-if="connection.id in form.access.mcpTools">
                      <label v-for="tool in connection.tools" :key="tool.name" class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="form.access.mcpTools[connection.id]" type="checkbox" :value="tool.name">{{ tool.name }}</label><small v-if="!connection.tools.length">Test this connection in MCPs to discover its tools.</small>
                    </template>
                  </details>
                </div>
                <small v-if="!state.mcps.length">Add connections in MCPs first.</small>
              </fieldset>
              <label class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="form.access.github" type="checkbox" :disabled="!allProjects">Shared GitHub connection</label>
              <label v-if="!form.access.github">Dedicated GitHub token<input v-model="githubToken" type="password" autocomplete="new-password" :placeholder="githubConfigured ? 'Saved token · leave blank to keep' : 'Optional fine-grained GitHub token'"><small>Select only this agent’s repositories and permissions when creating the token on GitHub. Its remote permissions are determined by the token.</small></label>
              <label v-if="githubConfigured && !form.access.github" class="checkbox flex-row items-center gap-2 text-xs font-normal phone:text-xs phone:leading-[1.6] mx-0 my-[9px]"><input v-model="removeGithub" type="checkbox">Remove saved GitHub token</label>
              <small>Shared GitHub credentials can access other repositories, so they are available only to agents with all-project access. Restricted agents receive only their selected MCP connections.</small>
            </template>
            <p v-else>
              The main agent has access to all registered projects, skills, and shared connections.
            </p>
            <label class="checkbox"><input v-model="allNodes" type="checkbox">All nodes, including future nodes</label>
            <fieldset v-if="!allNodes" class="access-choices">
              <legend>Allowed nodes</legend>
              <label class="checkbox"><input v-model="form.access.nodes" type="checkbox" :value="LOCAL_NODE_ID">Current runner</label>
              <label v-for="node in nodes.filter(n => !n.local && !n.revoked)" :key="node.id" class="checkbox"><input v-model="form.access.nodes" type="checkbox" :value="node.id">{{ node.name }}</label>
            </fieldset>
            <VirtualSelect v-model="form.access.sandbox" label="Execution mode" :options="sandboxOptions" :icon="ShieldCheck" />
            <p class="muted text-muted">
              YOLO is the default. Every agent runs in a private VM. Sandboxed runs never bypass denied operations or wait for unattended approvals.
            </p>
          </div><label class="checkbox flex-row items-center gap-2">
            <input
              type="checkbox"
              :checked="form.timeoutMinutes === 0"
              @change="form.timeoutMinutes = ($event.target as HTMLInputElement).checked ? 0 : 120"
            >No time limit
          </label><label v-if="form.timeoutMinutes !== 0">Time limit (minutes)<input
            v-model.number="form.timeoutMinutes"
            type="number"
            min="1"
            max="720"
            required
          ></label><label class="span-2 col-span-2 phone:col-span-1">Additional instructions<textarea
            v-model="form.instructions"
            rows="4"
            placeholder="Conventions, responsibilities, or checks this agent should always follow."
          />
          </label>
        </template><template v-else>
          <VirtualSelect v-if="!githubImport" v-model="form.sourceMode" label="New work starts from" :options="[{ value: 'remote', label: 'Latest remote branch', description: 'Fetch a fresh copy; local files stay untouched' }, { value: 'local', label: 'Local branch snapshot', description: 'Use committed files from the local branch' }]" />
          <label v-if="!githubImport" class="span-2 col-span-2 phone:col-span-1">Project directory<input
            v-model="form.path"
            required
            placeholder="/workspaces/my-project"
          ><small>Must be inside one of your configured workspace roots.</small></label><label class="span-2 col-span-2 phone:col-span-1">Base branch<input
            v-model="form.baseBranch"
            required
            placeholder="main"
          ><small>Used for new private workspaces.</small></label>
        </template>
        <UiAlert v-if="error" class="col-span-2 phone:col-span-1">
          {{ error }}
        </UiAlert>
      </div>
      <footer class="modal-actions flex justify-end gap-2.5 bg-surface border-t border-line sticky bottom-0 phone:flex-wrap px-6.5 py-4.5 phone:px-5 phone:py-4">
        <UiButton type="button" :disabled="busy" @click="open = false">
          Cancel
        </UiButton><UiButton variant="primary" type="submit" :disabled="busy || (githubImport && !repository)">
          {{ busy ? "Saving…" : `Save ${isAgent ? "agent" : "project"}` }}
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

<style scoped>
.agent-access-panel { display: grid; gap: 1rem; padding: 1.2rem; border: 1px solid var(--color-line); border-radius: 16px; background: var(--color-soft); }
.agent-access-heading { display: flex; gap: .75rem; align-items: center; }
.agent-access-heading h3, .agent-access-heading p { margin: 0; }
.agent-access-heading p { margin-top: .25rem; color: var(--color-muted); font-size: .85rem; }
.access-choices { max-height: 230px; overflow: auto; display: grid; gap: .6rem; padding: .75rem; }
</style>
