<script setup lang="ts">
import type { Agent } from '../../../../packages/contracts/contracts'
import type { CreateAgentInput } from '../../../../packages/contracts/generated/typescript/CreateAgentInput'
import type { ReplaceAgentInput } from '../../../../packages/contracts/generated/typescript/ReplaceAgentInput'
import type { ExecutionNode } from '../../../../packages/contracts/nodes'
import type {
  AgentDraft,
  ChipTone,
  Context,
  DelegationEffect,
  Preview,
  ReviewLine,
  SectionId,
  SelectionItem,
} from '../agent-access'
import type { Catalog } from '../plugins'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { onBeforeRouteLeave, useRoute, useRouter } from 'vue-router'
import { providers as codingAgents } from '../../../../packages/contracts/accounts'
import { MAIN_AGENT_ID } from '../../../../packages/contracts/constants'
import { createAgent } from '../../../../packages/contracts/generated/createAgent'
import { deleteAgent } from '../../../../packages/contracts/generated/deleteAgent'
import { listAgents } from '../../../../packages/contracts/generated/listAgents'
import { listPluginPackages } from '../../../../packages/contracts/generated/listPluginPackages'
import { previewAgent } from '../../../../packages/contracts/generated/previewAgent'
import { replaceAgent } from '../../../../packages/contracts/generated/replaceAgent'
import { LOCAL_NODE_ID } from '../../../../packages/contracts/nodes'
import {
  alwaysAvailable,
  backendOf,
  changes,
  draftOf,
  effectiveExclusive,
  errorSection,
  issues as findIssues,
  grantablePackages,
  isolationNote,
  mainLockedSections,
  nativeReviewNodes,
  needsReview,
  newAgentDraft,
  nodeName,
  nodesFor,
  rebaseDraft,
  reviewLines,
  sandboxOptionsFor,
  savedDraft,
  selectionSummary,
  skillKey,
  skillLabel,
} from '../agent-access'
import { useAgentPeople } from '../agent-people'
import { accessSummary } from '../agent-use'
import {
  api,
  ApiError,
  notify,
  refresh,
  state,
} from '../api'
import AccessChip from '../components/AccessChip.vue'
import AgentAvatar from '../components/AgentAvatar.vue'
import AgentMcpSection from '../components/AgentMcpSection.vue'
import AgentNodesSection from '../components/AgentNodesSection.vue'
import AgentOverview from '../components/AgentOverview.vue'
import AgentPeopleSection from '../components/AgentPeopleSection.vue'
import AgentPluginsSection from '../components/AgentPluginsSection.vue'
import AgentPortraitEditor from '../components/AgentPortraitEditor.vue'
import AgentReview from '../components/AgentReview.vue'
import AgentSelection from '../components/AgentSelection.vue'
import AssistantPicker from '../components/AssistantPicker.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import UiSwitch from '../components/UiSwitch.vue'
import {
  ArrowLeft,
  ChevronRight,
  Info,
  Lock,
  MessageCircle,
  Trash2,
  TriangleAlert,
} from '../icons'
import { useResourceSave } from '../resourceSave'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'

const route = useRoute()
const router = useRouter()
const id = computed(() => String(route.params.id))
const isNew = computed(() => id.value === 'new')
// A new agent keeps one creation identity, so retrying after a lost response never duplicates it.
const creation = useResourceSave()
const isMain = computed(() => id.value === MAIN_AGENT_ID)
const titles: Record<SectionId, string> = {
  identity: 'Identity & instructions',
  model: 'Coding agent & model',
  people: 'Who can use it',
  projects: 'Projects',
  skills: 'Skills',
  mcps: 'MCP & tools',
  plugins: 'Secrets & plugins',
  nodes: 'Nodes',
  sandbox: 'Execution mode',
  powers: 'Elevated rights',
  advanced: 'Limits',
}
const navGroups: { title: string, items: SectionId[] }[] = [
  { title: 'Profile', items: ['identity', 'model'] },
  { title: 'People', items: ['people'] },
  { title: 'Resource access', items: ['projects', 'skills', 'mcps', 'plugins'] },
  { title: 'Execution', items: ['nodes', 'sandbox'] },
  { title: 'Elevated rights', items: ['powers'] },
  { title: 'Advanced', items: ['advanced'] },
]
const section = computed<SectionId | undefined>(() => {
  const value = String(route.query.section ?? '')
  return value in titles ? value as SectionId : undefined
})

const loaded = ref(false)
const loadError = ref('')
const notFound = ref(false)
const agent = ref<Agent | null>(null)
const saved = ref<AgentDraft>(newAgentDraft())
const draft = ref<AgentDraft>(newAgentDraft())
const nodes = ref<ExecutionNode[]>([])
const catalog = ref<Catalog>({ packages: [], capabilities: [], connections: [] })
// Only a loaded catalogue says which grants exist: on failure, plugin access is shown read-only.
const catalogError = ref('')
// Never restrict from an empty list: when nodes cannot be loaded, node access is read-only.
const nodesError = ref('')
const nodesLoading = ref(false)
const catalogLoading = ref(false)
const suspended = ref<string[]>([])
const busy = ref(false)
const serverError = ref('')
const conflict = ref(false)
const attempted = ref(false)
const fieldErrors = ref<Partial<Record<SectionId, string>>>({})
const review = ref<{ lines: ReviewLine[], delegations: DelegationEffect[], nativeNodes: string[] } | null>(null)
const reviewError = ref('')
const leaving = ref(false)
const pendingLeave = ref<string | null>(null)
const deleting = ref(false)
const heading = ref<HTMLElement>()
// Account access applies immediately and never joins the unsaved profile draft.
const people = useAgentPeople(computed(() => isNew.value || notFound.value ? null : id.value))
// Until access loads, the overview neither claims the agent is private nor shared.
const peopleLoading = computed(() => !isNew.value && !people.error.value && !people.outdated.value && !people.loaded.value)
const peopleSummary = computed(() => isNew.value
  ? { short: 'Only you', long: 'Only you' }
  : people.outdated.value
    ? { short: 'Everyone', long: 'Every member' }
    : people.error.value
      ? { short: 'Unavailable', long: 'Not loaded' }
      : !people.loaded.value
          ? { short: '', long: 'Loading…' }
          : accessSummary(people.count.value))

const context = computed<Context>(() => ({
  agentId: agent.value?.id,
  projects: state.projects,
  skills: state.skills,
  mcps: state.mcps,
  nodes: nodes.value,
  packages: catalog.value.packages,
  connections: catalog.value.connections,
  pluginsUnavailable: Boolean(catalogError.value),
  nodesUnavailable: Boolean(nodesError.value),
  workspaceUnavailable: !state.workspaceLoaded,
}))
const access = computed(() => draft.value.access)
const backend = computed(() => backendOf(access.value))
// Nodes of the agent's execution backend, the only ones it can be granted.
const availableNodes = computed(() => nodesFor(nodes.value, backend.value))
const changed = computed(() => changes(saved.value, draft.value))
const dirty = computed(() => loaded.value && !notFound.value && changed.value.length > 0)
const issues = computed(() => loaded.value ? findIssues(draft.value, context.value, isMain.value, isNew.value ? null : id.value) : [])

const allProjects = computed({
  get: () => draft.value.access.projects,
  set: (value: string[] | null) => draft.value.access.projects = value,
})
const projectItems = computed<SelectionItem[]>(() => state.projects.map(project => ({ id: project.id, name: project.name, detail: project.description || undefined })))
const permittedSkills = computed(() => state.skills.filter(skill => skill.valid && (skill.scope === 'global' || access.value.projects === null || access.value.projects.includes(skill.scope))))
const skillItems = computed<SelectionItem[]>(() => {
  const chosen = new Set(access.value.skills ?? [])
  const shown = state.skills.filter(skill => permittedSkills.value.includes(skill) || (skill.valid && chosen.has(skillKey(skill))))
  return shown.map((skill) => {
    const { origin } = skillLabel(skillKey(skill), state.projects)
    const ambiguous = shown.filter(other => other.name === skill.name).length > 1
    return {
      id: skillKey(skill),
      name: skill.name,
      label: ambiguous ? `${skill.name} (${origin})` : skill.name,
      detail: origin,
      unavailable: permittedSkills.value.includes(skill) ? undefined : 'Project not allowed',
    }
  })
})

function names(ids: string[], name: (id: string) => string, limit = 3) {
  const listed = ids.slice(0, limit).map(name)
  return ids.length > limit ? `${listed.join(', ')} +${ids.length - limit}` : listed.join(', ')
}

function short(value: string[] | null, total: number) {
  return value === null ? 'All' : value.length ? `${value.length}/${Math.max(total, value.length)}` : 'None'
}

function chip(value: string[] | null): { tone: ChipTone, label: string } {
  return value === null || value.length ? { tone: 'granted', label: 'Granted' } : { tone: 'none', label: 'None' }
}

const projectName = (project: string) => state.projects.find(item => item.id === project)?.name ?? 'Removed project'
const connectionName = (connection: string) => catalog.value.connections.find(item => item.id === connection)?.name ?? 'Removed instance'
const provider = computed(() => codingAgents[draft.value.provider] ?? codingAgents.codex)
const modes = computed(() => sandboxOptionsFor(backendOf(access.value)))
const sandbox = computed(() => modes.value.find(option => option.value === access.value.sandbox) ?? modes.value[0]!)
// Only grants that take effect: the agent may run there and the runtime supports exclusive reservations.
const exclusiveNodes = computed(() => effectiveExclusive(draft.value, nodes.value))
const delegations = computed(() => isNew.value ? [] : catalog.value.connections.filter(connection => connection.agentId === id.value))
const pluginSummary = computed(() => {
  const counts = new Map<string, number>()
  for (const granted of draft.value.plugins) {
    const connection = catalog.value.connections.find(item => item.id === granted)
    const name = catalog.value.packages.find(entry => entry.id === connection?.pluginId)?.manifest?.name ?? 'Plugin'
    counts.set(name, (counts.get(name) ?? 0) + 1)
  }

  return [...counts].map(([name, count]) => name === '1Password' ? `1Password: ${count} ${count === 1 ? 'account' : 'accounts'}` : count > 1 ? `${name} (${count})` : name).join(' · ')
})
const powers = computed(() => exclusiveNodes.value.length + Number(access.value.installation) + Number(access.value.delegation))
const mcpCount = computed(() => access.value.mcps === null ? state.mcps.length : access.value.mcps.length)
// Connections whose tools are limited to a chosen few, also under "All, including future".
const limitedMcps = computed(() => state.mcps.filter(connection => connection.id in access.value.mcpTools && (access.value.mcps === null || access.value.mcps.includes(connection.id))))
const disabledMcps = computed(() => state.mcps.filter(connection => !connection.enabled && (access.value.mcps === null || access.value.mcps.includes(connection.id))))

// One line per section: short values beside the desktop navigation, longer ones on phones and cards.
const summaries = computed<Record<SectionId, { short: string, long: string }>>(() => ({
  identity: { short: '', long: draft.value.description || 'Name, description, portrait and instructions' },
  model: { short: provider.value.label, long: [provider.value.label, draft.value.model || 'Default model'].join(' · ') },
  people: peopleSummary.value,
  projects: !isMain.value && !state.workspaceLoaded ? { short: 'Unavailable', long: 'Not loaded' } : { short: isMain.value ? 'Fixed' : short(access.value.projects, state.projects.length), long: isMain.value ? 'All, including future' : selectionSummary(access.value.projects, state.projects.length) },
  skills: !isMain.value && !state.workspaceLoaded ? { short: 'Unavailable', long: 'Not loaded' } : { short: isMain.value ? 'Fixed' : short(access.value.skills, permittedSkills.value.length), long: isMain.value ? 'All, including future' : selectionSummary(access.value.skills, permittedSkills.value.length) },
  mcps: !isMain.value && !state.workspaceLoaded ? { short: 'Unavailable', long: 'Not loaded' } : { short: isMain.value ? 'Fixed' : short(access.value.mcps, state.mcps.length), long: isMain.value ? 'All, including future' : `${selectionSummary(access.value.mcps, state.mcps.length)}${limitedMcps.value.length ? ` · tools limited on ${limitedMcps.value.length}` : ''}${disabledMcps.value.length ? ` · ${disabledMcps.value.length} disabled` : ''}` },
  plugins: catalogError.value
    ? { short: 'Unavailable', long: 'Not loaded' }
    : { short: draft.value.plugins.length ? String(draft.value.plugins.length) : 'None', long: pluginSummary.value || 'None' },
  nodes: nodesError.value
    ? { short: 'Unavailable', long: 'Not loaded' }
    : { short: `${backend.value === 'native' ? 'Native · ' : ''}${short(access.value.nodes, availableNodes.value.length)}`, long: `${backend.value === 'native' ? 'Native process · ' : ''}${selectionSummary(access.value.nodes, availableNodes.value.length)}${exclusiveNodes.value.length ? ` · exclusive on ${exclusiveNodes.value.length}` : ''}` },
  sandbox: { short: sandbox.value.label, long: sandbox.value.label },
  powers: { short: powers.value ? String(powers.value) : nodesError.value ? 'Unavailable' : 'None', long: [nodesError.value ? 'Exclusive reservation not loaded' : exclusiveNodes.value.length ? 'Exclusive reservation' : '', access.value.installation ? 'Installation management' : '', access.value.delegation ? 'Task delegation' : ''].filter(Boolean).join(' · ') || 'None' },
  advanced: { short: draft.value.timeoutMinutes ? `${draft.value.timeoutMinutes} min` : '', long: draft.value.timeoutMinutes ? `Time limit: ${draft.value.timeoutMinutes} minutes` : 'No time limit' },
}))

const overview = computed(() => [
  {
    title: 'What this agent can access',
    cards: [
      {
        id: 'projects',
        section: 'projects' as const,
        title: 'Projects',
        summary: summaries.value.projects.long,
        detail: access.value.projects === null || isMain.value ? `${state.projects.length} today` : names(access.value.projects, projectName),
        chip: isMain.value ? { tone: 'fixed' as const, label: 'Fixed' } : chip(access.value.projects),
      },
      {
        id: 'skills',
        section: 'skills' as const,
        title: 'Skills',
        summary: summaries.value.skills.long,
        detail: access.value.skills === null || isMain.value ? `${permittedSkills.value.length} available · limited to allowed projects` : names(access.value.skills, key => skillLabel(key, state.projects).name),
        chip: isMain.value ? { tone: 'fixed' as const, label: 'Fixed' } : chip(access.value.skills),
      },
      {
        id: 'mcps',
        section: 'mcps' as const,
        title: 'MCP & tools',
        summary: isMain.value ? 'All, including future' : `${mcpCount.value} ${mcpCount.value === 1 ? 'connection' : 'connections'}${access.value.mcps === null ? ', including future' : ''}`,
        detail: isMain.value ? 'All enabled tools' : access.value.mcps === null ? (limitedMcps.value.length ? `Tools limited on ${names(limitedMcps.value.map(connection => connection.name), name => name)}` : 'All enabled tools') : names(access.value.mcps, connection => state.mcps.find(item => item.id === connection)?.name ?? 'Removed connection'),
        chip: isMain.value ? { tone: 'fixed' as const, label: 'Fixed' } : disabledMcps.value.length ? { tone: 'unavailable' as const, label: `${disabledMcps.value[0]!.name} disabled` } : chip(access.value.mcps),
      },
      {
        id: 'plugins',
        section: 'plugins' as const,
        title: 'Secrets & plugins',
        summary: catalogError.value ? 'Not loaded' : pluginSummary.value || 'No access',
        detail: catalogError.value ? 'Plugins could not be loaded' : [names(draft.value.plugins, connectionName), ...delegations.value.map(item => `Answers on ${item.name} (delegation)`)].filter(Boolean).join(' · ') || 'No plugin access by default, including Main',
        chip: catalogError.value ? { tone: 'unavailable' as const, label: 'Unavailable' } : draft.value.plugins.length ? { tone: 'granted' as const, label: 'Granted' } : { tone: 'none' as const, label: 'None' },
      },
    ],
  },
  {
    title: 'What it can do',
    cards: [
      {
        id: 'nodes',
        section: 'nodes' as const,
        title: 'Execution',
        summary: nodesError.value ? 'Not loaded' : `${backend.value === 'native' ? 'Native process · ' : ''}${access.value.nodes === null ? 'All nodes, including future' : `${access.value.nodes.length} of ${availableNodes.value.length} ${availableNodes.value.length === 1 ? 'node' : 'nodes'}`}`,
        detail: nodesError.value ? 'Nodes could not be loaded' : access.value.nodes === null ? `${availableNodes.value.length} today` : names(access.value.nodes, node => nodeName(nodes.value.find(item => item.id === node), node)) || 'Cannot run anywhere',
        chip: nodesError.value
          ? { tone: 'unavailable' as const, label: 'Unavailable' }
          : backend.value === 'native'
            ? { tone: 'power' as const, label: 'Native process' }
            : access.value.nodes?.length === 0 ? { tone: 'none' as const, label: 'None' } : { tone: 'granted' as const, label: 'Granted' },
      },
      {
        id: 'exclusive',
        section: 'nodes' as const,
        title: 'Exclusive reservation',
        summary: nodesError.value ? 'Not loaded' : exclusiveNodes.value.length ? names(exclusiveNodes.value, node => nodeName(nodes.value.find(item => item.id === node), node)) : 'None',
        detail: nodesError.value ? 'Nodes could not be loaded' : exclusiveNodes.value.length ? 'Can suspend other conversations for up to 15 minutes' : 'Cannot suspend other conversations',
        chip: nodesError.value ? { tone: 'unavailable' as const, label: 'Unavailable' } : exclusiveNodes.value.length ? { tone: 'power' as const, label: 'Power' } : undefined,
        power: true,
      },
      {
        id: 'installation',
        section: 'powers' as const,
        title: 'Installation management',
        summary: access.value.installation ? 'On' : 'Off',
        detail: access.value.installation ? 'Manages agents, projects, skills, MCP connections, missions and runs' : 'Cannot manage agents, projects or missions',
        chip: access.value.installation ? { tone: 'power' as const, label: 'Power' } : undefined,
        power: true,
      },
      {
        id: 'delegation',
        section: 'powers' as const,
        title: 'Task delegation',
        summary: access.value.delegation ? 'On' : 'Off',
        detail: access.value.delegation ? 'Starts conversations on other coding agents and models that report back to it' : 'Cannot hand tasks to other conversations',
        chip: access.value.delegation ? { tone: 'power' as const, label: 'Power' } : undefined,
        power: true,
      },
      {
        id: 'sandbox',
        section: 'sandbox' as const,
        title: 'Execution mode',
        summary: sandbox.value.label,
        detail: sandbox.value.description,
      },
      {
        id: 'model',
        section: 'model' as const,
        title: 'Coding agent & model',
        summary: summaries.value.model.long,
        detail: `Uses the shared ${provider.value.label} account pool`,
      },
      {
        id: 'always',
        section: 'powers' as const,
        title: 'Always available',
        summary: 'Conversation tools',
        detail: alwaysAvailable,
        action: 'View',
      },
    ],
  },
  {
    title: 'Who can use it',
    cards: [
      {
        id: 'people',
        section: 'people' as const,
        title: 'People',
        summary: peopleSummary.value.long,
        detail: peopleLoading.value
          ? 'Checking which members can use it'
          : people.outdated.value
            ? 'Update the installation to choose who can use it'
            : people.error.value
              ? 'Who can use it could not be loaded'
              : people.count.value
                ? names(people.rows.value.filter(row => row.known && row.allowed).map(row => row.email), email => email)
                : 'Members cannot start or continue its conversations',
        chip: people.outdated.value
          ? { tone: 'granted' as const, label: 'Everyone' }
          : people.error.value
            ? { tone: 'unavailable' as const, label: 'Unavailable' }
            : peopleLoading.value
              ? undefined
              : people.count.value ? { tone: 'granted' as const, label: 'Shared' } : { tone: 'none' as const, label: 'Only you' },
        action: 'Manage',
      },
    ],
  },
])

// Until projects, skills and MCP connections load, their cards say so instead of guessing.
const overviewShown = computed(() => isMain.value || state.workspaceLoaded
  ? overview.value
  : overview.value.map(group => ({
      ...group,
      cards: group.cards.map(card => ['projects', 'skills', 'mcps'].includes(card.id)
        ? {
            ...card,
            summary: 'Not loaded',
            detail: 'Could not be loaded',
            chip: { tone: 'unavailable' as const, label: 'Unavailable' },
          }
        : card),
    })))

function apply(found: Agent) {
  agent.value = found
  saved.value = draftOf(found)
  draft.value = draftOf(found)
  fieldErrors.value = {}
  attempted.value = false
}

async function loadSuspension() {
  if (!delegations.value.length || !agent.value)
    return
  try {
    const preview = await previewAgent(api, {
      agentId: id.value,
      revision: agent.value!.revision,
      settings: saved.value,
      review: { nativeNodeReviews: [], reapproveDelegations: [] },
    })
    suspended.value = preview.delegations.filter(item => item.suspended).map(item => item.id)
  }
  catch {}
}

// Reloads projects, skills and MCP connections; a failure leaves them flagged as not loaded.
async function loadWorkspace() {
  await refresh().catch(() => {})
}

const workspaceLoading = ref(false)

async function retryWorkspace() {
  workspaceLoading.value = true
  try {
    await loadWorkspace()
  }
  finally {
    workspaceLoading.value = false
  }
}

// An installation without plugins still edits every other permission.
async function loadCatalog() {
  try {
    const loadedCatalog = await listPluginPackages(api, {})
    catalogError.value = ''
    return loadedCatalog
  }
  catch (e) {
    catalogError.value = (e as Error).message || 'Plugins could not be loaded.'
    return null
  }
}

async function retryCatalog() {
  catalogLoading.value = true
  try {
    const plugins = await loadCatalog()
    if (plugins) {
      catalog.value = plugins
      void loadSuspension()
    }
  }
  finally {
    catalogLoading.value = false
  }
}

// Like plugins, nodes that cannot be loaded leave the rest of the page usable.
async function loadNodes() {
  try {
    const nodeList = await api<ExecutionNode[]>('/nodes')
    nodesError.value = ''
    return nodeList
  }
  catch (e) {
    nodesError.value = (e as Error).message || 'Nodes could not be loaded.'
    return null
  }
}

async function retryNodes() {
  nodesLoading.value = true
  try {
    const nodeList = await loadNodes()
    if (nodeList) {
      nodes.value = nodeList
    }
  }
  finally {
    nodesLoading.value = false
  }
}

async function load() {
  loadError.value = ''
  loaded.value = false
  try {
    const [agents, nodeList, plugins] = await Promise.all([
      listAgents(api, {}).then(output => output.agents),
      loadNodes(),
      // An installation without plugins still edits every other permission.
      loadCatalog(),
      // Usually loaded with the workspace already: only fetched again when it is not.
      state.workspaceLoaded ? Promise.resolve() : loadWorkspace(),
    ])
    if (nodeList)
      nodes.value = nodeList
    if (plugins)
      catalog.value = plugins
    if (isNew.value) {
      saved.value = newAgentDraft()
      draft.value = newAgentDraft()
    }
    else {
      const found = agents.find(item => item.id === id.value)
      if (!found) {
        notFound.value = true
        return
      }

      apply(found)
      void loadSuspension()
      void people.load()
    }
  }
  catch (e) {
    loadError.value = (e as Error).message
  }
  finally {
    loaded.value = true
  }
}

function validate() {
  const errors: Partial<Record<SectionId, string>> = {}
  if (!draft.value.name.trim())
    errors.identity = 'Enter a name for this agent.'
  const limit = draft.value.timeoutMinutes
  if (limit !== 0 && !(Number.isInteger(limit) && limit >= 1 && limit <= 720))
    errors.advanced = 'Enter a time limit between 1 and 720 minutes, or choose no time limit.'
  fieldErrors.value = errors
  return !Object.keys(errors).length
}

watch(draft, () => {
  if (attempted.value)
    validate()
}, { deep: true })

function body(reapprove: string[] = [], nativeNodes: string[] = []): ReplaceAgentInput {
  // Also set once a new agent exists: a recovery update must not overwrite another client's change.
  const revision = agent.value?.revision
  // Only a loaded connection list says which ones were deleted.
  const profile = savedDraft(draft.value, state.workspaceLoaded && state.installationRole === 'owner' ? state.mcps.map(connection => connection.id) : undefined)
  return {
    agentId: agent.value?.id ?? id.value,
    revision: revision ?? '',
    settings: profile,
    review: { reapproveDelegations: reapprove, nativeNodeReviews: nativeNodes },
  }
}

async function requestSave() {
  if (busy.value)
    return
  attempted.value = true
  serverError.value = ''
  conflict.value = false
  if (!validate()) {
    const first = Object.keys(fieldErrors.value)[0] as SectionId
    if (section.value !== first)
      await router.push({ query: { section: first } })
    return
  }

  let lines = reviewLines(saved.value, draft.value, context.value)
  let effects: DelegationEffect[] = []
  let current: Preview | null = null
  if (!isNew.value && changes(saved.value, draft.value).length) {
    busy.value = true
    try {
      // The server says which queued or running work and which delegations the change affects.
      const preview = await previewAgent(api, body())
      lines = reviewLines(saved.value, draft.value, context.value, preview.runs)
      effects = preview.delegations.filter(item => item.suspended)
      current = preview
    }
    catch (e) {
      failed(e)
      return
    }
    finally {
      busy.value = false
    }
  }

  // Powers, secrets, interrupted work and delegations this edit suspends deserve a look; anything
  // else saves directly.
  if (needsReview(lines, current)) {
    reviewError.value = ''
    review.value = { lines, delegations: effects, nativeNodes: nativeReviewNodes(draft.value, context.value) }
    return
  }

  await commit([])
}

function failed(e: unknown) {
  const message = (e as Error).message
  if (e instanceof ApiError && e.status === 409) {
    conflict.value = true
    review.value = null
  }

  if (review.value)
    reviewError.value = message
  else
    serverError.value = message
  const target = errorSection(message)
  if (target && !conflict.value)
    fieldErrors.value = { ...fieldErrors.value, [target]: message }
}

async function agentRequest<T>(_path: string, options: RequestInit): Promise<T> {
  const payload = JSON.parse(options.body as string)
  return (options.method === 'POST'
    ? await createAgent(api, payload as CreateAgentInput)
    : await replaceAgent(api, payload as ReplaceAgentInput)) as T
}

async function commit(reapprove: string[]) {
  const nativeNodes = review.value?.nativeNodes ?? []
  busy.value = true
  serverError.value = ''
  reviewError.value = ''
  // After a saved profile and a failed reload, a retry only reloads.
  const profileChanged = isNew.value || changes(saved.value, draft.value).length > 0
  let result: Agent
  try {
    if (isNew.value) {
      result = await creation.save<Agent>('/agents', {
        body: () => body(reapprove, nativeNodes),
        request: agentRequest,
        createBody: creationId => ({ creationId, settings: body(reapprove, nativeNodes).settings, review: body(reapprove, nativeNodes).review }),
        // A replayed creation updates the agent it created, at that agent's revision.
        onCreated: (created) => { agent.value = created },
      })
    }
    else if (profileChanged) {
      result = await replaceAgent(api, body(reapprove, nativeNodes))
    }
    else {
      result = agent.value!
    }
  }
  catch (e) {
    failed(e)
    const detail = (e as Error).message
    let summary = ''
    // Once the agent exists, any rejected update is about the newer edits only. A conflict keeps
    // its own message, which asks for a reload.
    if (conflict.value)
      summary = ''
    else if (isNew.value && creation.savedId.value)
      summary = 'Agent saved, but the latest changes could not be saved.'
    else if (isNew.value && creation.pendingCreation.value)
      summary = 'Could not confirm whether the agent was saved. Retry to recover it safely.'
    else if (isNew.value)
      summary = 'Could not save the agent.'
    if (summary)
      serverError.value = detail ? `${summary} ${detail}` : summary

    busy.value = false
    return
  }

  review.value = null
  // The saved profile and its revision advance now. A created agent stays on this draft until the
  // workspace is reloaded too: retrying then updates the same agent instead of creating another one.
  agent.value = result
  if (profileChanged) {
    saved.value = draftOf(result)
    draft.value = draftOf(result)
    fieldErrors.value = {}
    attempted.value = false
  }

  let completed = false
  try {
    await refresh()
    completed = true
    notify(isNew.value ? 'Agent created' : 'Agent saved')
  }
  catch (e) {
    serverError.value = `Agent saved, but the workspace could not be reloaded. ${(e as Error).message}`
  }
  finally {
    busy.value = false
  }

  if (isNew.value) {
    if (completed) {
      leaving.value = true
      await router.replace(`/agents/${result.id}`)
    }

    return
  }

  suspended.value = []
  void loadSuspension()
}

// Like Android, the owner's edits are kept on top of the newer agent: values they changed keep
// the draft, every other value takes the newer one, and the newer revision becomes the base.
async function reloadAgent() {
  const target = isNew.value ? creation.savedId.value : id.value
  busy.value = true
  try {
    // Nodes, plugins and MCP connections may have changed too, as the agent did.
    // The workspace refresh brings the agents too; they are only fetched alone if it fails.
    const [refreshed, nodeList, plugins] = await Promise.all([refresh().then(() => true, () => false), loadNodes(), loadCatalog()])
    const agents = refreshed ? state.agents : await listAgents(api, {}).then(output => output.agents)
    if (nodeList)
      nodes.value = nodeList
    if (plugins)
      catalog.value = plugins
    const found = agents.find(item => item.id === target)
    if (!found) {
      notFound.value = true
      return
    }

    const base = saved.value
    const mine = draft.value
    agent.value = found
    saved.value = draftOf(found)
    const rebased = rebaseDraft(base, mine, saved.value)
    // Tool choices of deleted MCP connections are dropped.
    if (state.workspaceLoaded && state.installationRole === 'owner')
      rebased.access.mcpTools = Object.fromEntries(Object.entries(rebased.access.mcpTools ?? {}).filter(([connection]) => state.mcps.some(item => item.id === connection)))
    draft.value = rebased
    void loadSuspension()
    conflict.value = false
    serverError.value = ''
  }
  catch (e) {
    serverError.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

function discard() {
  draft.value = structuredClone(JSON.parse(JSON.stringify(saved.value)))
  fieldErrors.value = {}
  attempted.value = false
  serverError.value = ''
  conflict.value = false
}

async function remove() {
  busy.value = true
  try {
    await deleteAgent(api, { agentId: id.value })
    await refresh()
    leaving.value = true
    notify('Agent removed')
    await router.push('/agents')
  }
  catch (e) {
    serverError.value = (e as Error).message
    deleting.value = false
  }
  finally {
    busy.value = false
  }
}

function open(target?: SectionId) {
  void router.push({ query: target ? { section: target } : {} })
}

onBeforeRouteLeave((to) => {
  if (leaving.value || !dirty.value || busy.value)
    return true
  pendingLeave.value = to.fullPath
  return false
})

function confirmLeave() {
  const target = pendingLeave.value
  pendingLeave.value = null
  leaving.value = true
  if (target)
    void router.push(target)
}

function beforeUnload(event: BeforeUnloadEvent) {
  if (dirty.value && !leaving.value)
    event.preventDefault()
}

// Focus follows the opened section so keyboard and screen reader users land on it.
watch(section, async () => {
  await nextTick()
  heading.value?.focus()
})

onMounted(async () => {
  window.addEventListener('beforeunload', beforeUnload)
  if (isNew.value && !section.value)
    await router.replace({ query: { section: 'identity' } })
  await load()
})
onBeforeUnmount(() => window.removeEventListener('beforeunload', beforeUnload))

const linkButton = [buttonBase, buttonVariants.default, buttonSizes.small].join(' ')
const errorCount = computed(() => Object.keys(fieldErrors.value).length)
</script>

<template>
  <div class="agent-page mx-auto flex w-full max-w-310 min-w-0 flex-col">
    <header class="mb-6 flex min-w-0 items-center gap-4 phone:mb-4 phone:gap-3">
      <RouterLink
        to="/agents"
        class="inline-flex min-h-11 shrink-0 items-center gap-1 rounded-lg text-sm text-muted hover:text-ink phone:min-w-11 phone:justify-center"
        aria-label="Back to agents"
      >
        <span class="hidden phone:inline-flex"><Icon :name="ArrowLeft" :size="18" /></span><span class="inline-flex items-center gap-1 phone:hidden">Agents<Icon :name="ChevronRight" :size="14" /></span>
      </RouterLink>
      <AgentAvatar :name="draft.name || 'New agent'" :identity="isNew ? '' : id" :size="44" />
      <div class="min-w-0 flex-1">
        <h1 class="truncate text-2xl! leading-tight! tracking-[-0.03em]! phone:text-xl!">
          {{ isNew ? (draft.name.trim() || 'New agent') : (saved.name || 'Agent') }}
        </h1>
        <p v-if="!isNew && saved.description" class="truncate text-sm text-muted">
          {{ saved.description }}
        </p>
      </div>
      <div v-if="!isNew && loaded && !loadError && !notFound" class="flex shrink-0 items-center gap-2">
        <RouterLink :to="{ path: '/chats', query: { agent: id } }" :class="linkButton" aria-label="Chat with this agent">
          <Icon :name="MessageCircle" :size="15" /><span class="phone:hidden">Chat</span>
        </RouterLink>
        <UiButton
          v-if="!isMain"
          size="small"
          variant="danger-outline"
          :aria-label="`Delete ${saved.name}`"
          @click="deleting = true"
        >
          <Icon :name="Trash2" :size="15" /><span class="phone:hidden">Delete</span>
        </UiButton>
      </div>
    </header>

    <div v-if="loadError" class="grid justify-items-start gap-3">
      <UiAlert class="my-0 w-full">
        {{ loadError }}
      </UiAlert>
      <UiButton size="small" @click="load">
        Try again
      </UiButton>
    </div>
    <div v-if="notFound" class="rounded-xl border border-dashed border-line p-6 text-sm text-muted">
      This agent no longer exists. <RouterLink to="/agents" class="text-accent hover:underline">
        Back to agents
      </RouterLink>
    </div>

    <div v-else-if="loaded && !loadError" class="grid min-w-0 flex-1 grid-cols-[232px_minmax(0,1fr)] items-start gap-8 phone:grid-cols-1 phone:gap-0">
      <nav class="sticky top-0 grid min-w-0 gap-0.5 short:static phone:static" :class="section ? 'phone:hidden' : ''" aria-label="Agent sections">
        <p v-if="isMain" class="mb-3 hidden rounded-xl bg-soft px-4 py-3 text-xs leading-relaxed phone:block">
          The main agent always has access to every project, skill and MCP connection, including future ones. Create another agent for restricted access.
        </p>
        <button
          v-if="issues.length"
          type="button"
          class="mb-3 hidden min-h-11 items-center gap-2 rounded-xl bg-warning-surface px-4 text-left text-sm text-warning phone:flex"
          @click="open(issues[0]!.section)"
        >
          <Icon :name="TriangleAlert" :size="16" />{{ issues.length }} {{ issues.length === 1 ? 'thing' : 'things' }} to check
        </button>
        <RouterLink
          :to="{ query: {} }"
          class="flex min-h-10 items-center rounded-lg px-3.5 text-sm font-semibold phone:hidden"
          :class="!section ? 'bg-soft text-accent' : 'text-muted hover:bg-hover hover:text-ink'"
          :aria-current="!section ? 'page' : undefined"
        >
          Overview
        </RouterLink>
        <template v-for="group in navGroups" :key="group.title">
          <p class="mt-4 mb-1 px-3.5 text-3xs font-semibold uppercase tracking-[.12em] text-subtle phone:mt-5 phone:px-1">
            {{ group.title }}
          </p>
          <RouterLink
            v-for="item in group.items"
            :key="item"
            :to="{ query: { section: item } }"
            class="flex min-h-10 min-w-0 items-center gap-2 rounded-lg px-3.5 text-sm phone:min-h-16 phone:rounded-none phone:border-b phone:border-line phone:px-1 phone:py-2.5"
            :class="section === item ? 'bg-soft font-semibold text-accent' : 'text-ink hover:bg-hover'"
            :aria-current="section === item ? 'page' : undefined"
          >
            <span class="min-w-0 flex-1">
              <span class="block truncate">{{ titles[item] }}</span>
              <span class="hidden truncate text-xs text-muted phone:block">{{ summaries[item].long }}</span>
            </span>
            <span
              v-if="fieldErrors[item]"
              class="size-2 shrink-0 rounded-full bg-danger"
              aria-label="Has an error"
              role="img"
            />
            <span
              v-else-if="changed.includes(item)"
              class="size-2 shrink-0 rounded-full bg-accent"
              aria-label="Changed"
              role="img"
            />
            <AccessChip v-if="isMain && mainLockedSections.includes(item)" tone="fixed" class="hidden phone:inline-flex">
              Fixed
            </AccessChip>
            <span v-else class="shrink-0 text-xs text-muted tabular-nums phone:hidden">{{ summaries[item].short }}</span>
            <span class="hidden shrink-0 text-muted phone:inline-flex"><Icon :name="ChevronRight" :size="16" /></span>
          </RouterLink>
        </template>
      </nav>

      <div class="min-w-0 pb-6" :class="!section ? 'phone:hidden' : ''">
        <RouterLink :to="{ query: {} }" class="mb-4 hidden min-h-11 items-center gap-1.5 text-sm text-muted hover:text-ink phone:inline-flex">
          <Icon :name="ArrowLeft" :size="16" />All settings
        </RouterLink>

        <template v-if="!section">
          <p v-if="isMain" class="mb-5 rounded-xl bg-soft px-5 py-3.5 text-sm leading-relaxed">
            The main agent always has access to every project, skill and MCP connection, including future ones. Create another agent for restricted access.
          </p>
          <div v-if="issues.length" class="mb-5 rounded-xl bg-warning-surface px-5 py-3.5 text-sm text-warning" role="status">
            <p class="flex items-center gap-2 font-semibold">
              <Icon :name="TriangleAlert" :size="16" />{{ issues.length }} {{ issues.length === 1 ? 'thing' : 'things' }} to check
            </p>
            <ul class="mt-1.5 grid gap-1">
              <li v-for="issue in issues" :key="issue.text">
                <button type="button" class="text-left underline-offset-2 hover:underline" @click="open(issue.section)">
                  {{ issue.text }}
                </button>
              </li>
            </ul>
          </div>
          <AgentOverview :groups="overviewShown" @open="open" />
        </template>

        <section v-else :aria-labelledby="`agent-section-${section}`" class="grid min-w-0 gap-5">
          <h2
            :id="`agent-section-${section}`"
            ref="heading"
            tabindex="-1"
            class="text-xl! font-heading outline-none"
          >
            {{ titles[section] }}
          </h2>
          <UiAlert v-if="fieldErrors[section]" class="my-0">
            {{ fieldErrors[section] }}
          </UiAlert>
          <p v-if="isMain && mainLockedSections.includes(section)" class="rounded-xl bg-soft px-5 py-3.5 text-sm leading-relaxed">
            The main agent always has access to every project, skill and MCP connection, including future ones. Create another agent for restricted access.
          </p>

          <template v-if="section === 'identity'">
            <AgentPortraitEditor
              :key="id"
              :agent-id="isNew ? null : id"
              :name="draft.name"
              :disabled="busy"
            />
            <label>Name<input
              v-model="draft.name"
              required
              maxlength="100"
              placeholder="e.g. Release engineer"
              :aria-invalid="Boolean(fieldErrors.identity)"
            ></label>
            <label>Description<textarea
              v-model="draft.description"
              rows="2"
              maxlength="500"
              placeholder="A short reminder of what this is for."
            /></label>
            <label>Additional instructions<textarea
              v-model="draft.instructions"
              rows="6"
              placeholder="Conventions, responsibilities, or checks this agent should always follow."
            /></label>
          </template>

          <template v-else-if="section === 'model'">
            <AssistantPicker
              v-model:provider="draft.provider"
              v-model:model="draft.model"
              v-model:reasoning="draft.reasoning"
              v-model:speed="draft.speed"
              variant="field"
              :disabled="busy"
            />
            <p class="flex items-start gap-2 rounded-xl bg-soft px-4 py-3 text-xs leading-relaxed">
              <Icon :name="Info" :size="15" class="mt-px shrink-0" />Uses the shared {{ provider.label }} account pool from Connections. Choosing a model grants no permission.
            </p>
          </template>

          <template v-else-if="section === 'people'">
            <AgentPeopleSection
              :agent-name="saved.name"
              :is-new="isNew"
              :installation="saved.access.installation || access.installation"
              :rows="people.rows.value"
              :invited="people.invited.value"
              :loading="people.loading.value"
              :loaded="people.loaded.value"
              :error="people.error.value"
              :outdated="people.outdated.value"
              :saving="people.saving.value"
              :failure="people.failure.value"
              @retry="people.load"
              @change="people.setAccess"
            />
          </template>

          <template v-else-if="isMain && mainLockedSections.includes(section)">
            <div class="flex items-center justify-between gap-3 rounded-xl border border-line bg-surface px-5 py-4">
              <span class="min-w-0">
                <span class="block text-sm font-semibold">{{ titles[section] }}</span>
                <span class="block text-xs text-muted">All, including future</span>
              </span>
              <AccessChip tone="fixed">
                Fixed
              </AccessChip>
            </div>
          </template>

          <template v-else-if="section === 'projects'">
            <div v-if="!state.workspaceLoaded" class="grid gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
              <p class="text-sm">
                Projects could not be loaded, so this agent’s projects cannot be shown or changed.
              </p>
              <p class="text-sm text-muted">
                {{ access.projects === null ? 'All, including future' : access.projects.length ? `${access.projects.length} selected` : 'None' }}
              </p>
              <div>
                <UiButton size="small" :disabled="workspaceLoading" @click="retryWorkspace">
                  {{ workspaceLoading ? 'Loading…' : 'Try again' }}
                </UiButton>
              </div>
            </div>
            <AgentSelection
              v-else
              v-model="allProjects"
              title="Projects"
              noun="project"
              :items="projectItems"
            >
              <template #all>
                <p>Every project, including those added later.</p>
              </template>
              <template #empty>
                No projects registered yet. <RouterLink to="/projects" class="text-accent hover:underline">
                  Add one in Projects ›
                </RouterLink>
              </template>
              <template #missing>
                Removed project
              </template>
            </AgentSelection>
          </template>

          <template v-else-if="section === 'skills'">
            <div v-if="!state.workspaceLoaded" class="grid gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
              <p class="text-sm">
                Skills could not be loaded, so this agent’s skills cannot be shown or changed.
              </p>
              <p class="text-sm text-muted">
                {{ access.skills === null ? 'All, including future' : access.skills.length ? `${access.skills.length} selected` : 'None' }}
              </p>
              <div>
                <UiButton size="small" :disabled="workspaceLoading" @click="retryWorkspace">
                  {{ workspaceLoading ? 'Loading…' : 'Try again' }}
                </UiButton>
              </div>
            </div>
            <AgentSelection
              v-else
              v-model="draft.access.skills"
              title="Skills"
              noun="skill"
              :items="skillItems"
            >
              <template #all>
                <p>Every valid skill, including future ones, limited to global skills and those of allowed projects.</p>
              </template>
              <template #empty>
                No skill is available to this agent. Global skills and skills of allowed projects appear here.
              </template>
              <template #missing="{ id: key }">
                {{ skillLabel(key, state.projects).name }} · {{ skillLabel(key, state.projects).origin }}
              </template>
            </AgentSelection>
          </template>

          <template v-else-if="section === 'mcps'">
            <div v-if="!state.workspaceLoaded" class="grid gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
              <p class="text-sm">
                MCP connections could not be loaded, so this agent’s MCP connections cannot be shown or changed.
              </p>
              <p class="text-sm text-muted">
                {{ access.mcps === null ? 'All, including future' : access.mcps.length ? `${access.mcps.length} selected` : 'None' }}{{ Object.keys(access.mcpTools ?? {}).length ? ` · tools limited on ${Object.keys(access.mcpTools).length}` : '' }}
              </p>
              <div>
                <UiButton size="small" :disabled="workspaceLoading" @click="retryWorkspace">
                  {{ workspaceLoading ? 'Loading…' : 'Try again' }}
                </UiButton>
              </div>
            </div>
            <AgentMcpSection v-else v-model:access="draft.access" :mcps="state.mcps" />
          </template>

          <template v-else-if="section === 'plugins'">
            <div v-if="catalogError" class="grid gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
              <p class="text-sm">
                Plugins could not be loaded, so this agent’s plugin access cannot be shown or changed. {{ catalogError }}
              </p>
              <ul v-if="draft.plugins.length" class="grid gap-1 text-sm text-muted" aria-label="Held plugin access">
                <li v-for="grant in draft.plugins" :key="grant">
                  Granted instance {{ grant.slice(0, 8) }}…
                </li>
              </ul>
              <div>
                <UiButton size="small" :disabled="catalogLoading" @click="retryCatalog">
                  {{ catalogLoading ? 'Loading…' : 'Try again' }}
                </UiButton>
              </div>
            </div>
            <AgentPluginsSection
              v-else
              v-model:plugins="draft.plugins"
              :agent-id="isNew ? null : id"
              :packages="catalog.packages"
              :connections="catalog.connections"
              :suspended="suspended"
            />
            <p v-if="!catalogError && !grantablePackages(catalog.packages).length && !catalog.packages.length" class="text-xs text-muted">
              Plugins add secrets and tools that you grant per agent.
            </p>
          </template>

          <template v-else-if="section === 'nodes'">
            <div v-if="nodesError" class="grid gap-3 rounded-xl border border-line bg-surface p-5" role="alert">
              <p class="text-sm">
                Nodes could not be loaded, so this agent’s node access cannot be shown or changed. {{ nodesError }}
              </p>
              <ul class="grid gap-1 text-sm text-muted" aria-label="Held node access">
                <li v-if="access.nodes === null">
                  Can run on all nodes, including future
                </li>
                <li v-for="node in access.nodes ?? []" :key="`run-${node}`">
                  Can run on {{ node === LOCAL_NODE_ID ? 'the current runner' : `node ${node.slice(0, 8)}…` }}
                </li>
                <li v-if="access.nodes?.length === 0">
                  Cannot run on any node
                </li>
                <li v-for="node in draft.exclusiveNodes" :key="`exclusive-${node}`">
                  Can reserve {{ node === LOCAL_NODE_ID ? 'the current runner' : `node ${node.slice(0, 8)}…` }} exclusively
                </li>
              </ul>
              <div>
                <UiButton size="small" :disabled="nodesLoading" @click="retryNodes">
                  {{ nodesLoading ? 'Loading…' : 'Try again' }}
                </UiButton>
              </div>
            </div>
            <AgentNodesSection
              v-else
              v-model:access="draft.access"
              v-model:exclusive-nodes="draft.exclusiveNodes"
              v-model:node-priorities="draft.nodePriorities"
              :nodes="nodes"
              :agent-id="agent?.id"
            />
          </template>

          <template v-else-if="section === 'sandbox'">
            <fieldset class="grid gap-2.5">
              <legend class="sr-only">
                Execution mode
              </legend>
              <label
                v-for="option in modes"
                :key="option.value"
                class="grid! grid-cols-[auto_1fr] items-start gap-x-3! gap-y-0.5! rounded-xl border px-4 py-3.5 font-normal! text-ink!"
                :class="access.sandbox === option.value ? 'border-accent bg-soft/50' : 'border-line bg-surface'"
              >
                <input
                  v-model="draft.access.sandbox"
                  type="radio"
                  class="mt-1 size-4 w-4!"
                  :value="option.value"
                ><span class="text-sm font-semibold">{{ option.label }}</span>
                <small class="col-start-2">{{ option.description }}</small>
              </label>
            </fieldset>
            <p class="text-xs text-muted">
              {{ isolationNote(backend) }}
            </p>
          </template>

          <template v-else-if="section === 'powers'">
            <div class="flex items-center justify-between gap-4 rounded-xl border border-power/30 bg-surface px-5 py-4 phone:px-4">
              <div class="min-w-0">
                <p class="flex flex-wrap items-center gap-2 text-sm font-semibold">
                  Installation management<AccessChip v-if="access.installation" tone="power">
                    Power
                  </AccessChip>
                </p>
                <p class="mt-1 text-xs leading-relaxed text-muted">
                  Create and change agents (including their permissions), projects, skills, MCP connections, missions and runs across the whole installation, on behalf of the owner. Applies even when the access above is restricted. Exclusive reservations and plugin grants stay with you.
                </p>
              </div>
              <UiSwitch v-model="draft.access.installation" power label="Installation management" />
            </div>
            <div class="flex items-center justify-between gap-4 rounded-xl border border-power/30 bg-surface px-5 py-4 phone:px-4">
              <div class="min-w-0">
                <p class="flex flex-wrap items-center gap-2 text-sm font-semibold">
                  Task delegation<AccessChip v-if="access.delegation" tone="power">
                    Power
                  </AccessChip>
                </p>
                <p class="mt-1 text-xs leading-relaxed text-muted">
                  Hand tasks from a conversation to new conversations that report back to it, on any coding agent and model. Each runs as this agent, or as another agent whose access this one covers, in its own workspace. Delegated conversations cannot delegate further, and at most 5 work at a time.
                </p>
              </div>
              <UiSwitch v-model="draft.access.delegation" power label="Task delegation" />
            </div>
            <div class="flex items-center justify-between gap-4 rounded-xl border border-power/30 bg-surface px-5 py-4 phone:px-4">
              <div class="min-w-0">
                <p class="text-sm font-semibold">
                  Exclusive reservation
                </p>
                <p class="mt-1 text-xs text-muted">
                  {{ nodesError ? 'Not loaded: nodes could not be loaded' : exclusiveNodes.length ? `Granted on ${names(exclusiveNodes, node => nodeName(nodes.find(item => item.id === node), node), 5)}` : 'Not granted on any node' }} ·
                  <RouterLink :to="{ query: { section: 'nodes' } }" class="text-accent hover:underline">
                    Set per node ›
                  </RouterLink>
                </p>
              </div>
              <AccessChip :tone="nodesError ? 'unavailable' : exclusiveNodes.length ? 'power' : 'none'">
                {{ nodesError ? 'Unavailable' : exclusiveNodes.length ? `${exclusiveNodes.length} ${exclusiveNodes.length === 1 ? 'node' : 'nodes'}` : 'None' }}
              </AccessChip>
            </div>
            <div class="rounded-xl bg-inset px-5 py-4 text-xs leading-relaxed text-muted">
              <p class="mb-1 flex items-center gap-1.5 font-semibold text-ink">
                <Icon :name="Lock" :size="13" />Always available, without settings
              </p>
              {{ alwaysAvailable }}
            </div>
          </template>

          <template v-else-if="section === 'advanced'">
            <label class="flex-row! items-center gap-2! text-sm font-normal! text-ink!">
              <input
                type="checkbox"
                class="size-4 w-4!"
                :checked="draft.timeoutMinutes === 0"
                @change="draft.timeoutMinutes = ($event.target as HTMLInputElement).checked ? 0 : 120"
              >No time limit
            </label>
            <label v-if="draft.timeoutMinutes !== 0" class="max-w-60">Time limit (minutes)<input
              v-model.number="draft.timeoutMinutes"
              type="number"
              min="1"
              max="720"
              required
              :aria-invalid="Boolean(fieldErrors.advanced)"
            ></label>
            <p class="text-xs text-muted">
              Runs that reach the limit stop.
            </p>
          </template>
        </section>
      </div>
    </div>

    <div
      v-if="loaded && !loadError && !notFound && (dirty || isNew || serverError || conflict)"
      class="sticky bottom-0 z-10 -mx-2 flex flex-wrap items-center gap-3 rounded-t-xl border border-b-0 border-line bg-raised px-5 py-3.5 shadow-lift phone:-mx-1 phone:px-4 phone:pb-[max(14px,env(safe-area-inset-bottom))]"
      role="region"
      aria-label="Unsaved changes"
    >
      <div class="min-w-0 flex-1 text-sm">
        <p v-if="conflict" class="text-danger" role="alert">
          {{ serverError || 'This agent was changed elsewhere. Reload it before saving.' }}
        </p>
        <p v-else-if="serverError" class="text-danger" role="alert">
          {{ serverError }}
        </p>
        <p v-else-if="errorCount" class="text-danger">
          Fix {{ errorCount === 1 ? 'this' : 'these' }} before saving:
          <template v-for="(message, key, index) in fieldErrors" :key="key">
            <RouterLink :to="{ query: { section: key } }" class="underline">
              {{ titles[key] }}
            </RouterLink><template v-if="index < errorCount - 1">
              ,
            </template>
          </template>
        </p>
        <p v-else-if="isNew" class="text-muted">
          The agent is created with these settings.
        </p>
        <p v-else class="flex items-center gap-2">
          <span class="size-2 rounded-full bg-warning" />{{ changed.length }} unsaved {{ changed.length === 1 ? 'change' : 'changes' }}
        </p>
      </div>
      <div class="flex shrink-0 gap-2 phone:w-full phone:justify-end">
        <template v-if="conflict">
          <UiButton :disabled="busy" @click="reloadAgent">
            Reload
          </UiButton>
        </template>
        <template v-else-if="isNew">
          <RouterLink to="/agents" :class="[buttonBase, buttonVariants.default, buttonSizes.default]">
            Cancel
          </RouterLink>
          <UiButton variant="primary" :disabled="busy" @click="requestSave">
            {{ busy ? 'Creating…' : 'Create agent' }}
          </UiButton>
        </template>
        <template v-else>
          <UiButton :disabled="busy || !dirty" @click="discard">
            Discard
          </UiButton>
          <UiButton variant="primary" :disabled="busy || !dirty" @click="requestSave">
            {{ busy ? 'Saving…' : 'Save' }}
          </UiButton>
        </template>
      </div>
    </div>

    <AgentReview
      v-if="review"
      :lines="review.lines"
      :delegations="review.delegations"
      :busy="busy"
      :error="reviewError"
      @close="review = null"
      @confirm="commit"
    />
    <Modal v-if="pendingLeave" title="Discard unsaved changes?" @close="pendingLeave = null">
      <div class="px-6.5 py-6 phone:p-5">
        <p class="text-sm">
          {{ changed.length }} unsaved {{ changed.length === 1 ? 'change' : 'changes' }} to this agent will be lost.
        </p>
      </div>
      <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <UiButton @click="pendingLeave = null">
          Keep editing
        </UiButton>
        <UiButton variant="danger" @click="confirmLeave">
          Discard changes
        </UiButton>
      </footer>
    </Modal>
    <Modal v-if="deleting" :title="`Delete ${saved.name}?`" @close="!busy && (deleting = false)">
      <div class="px-6.5 py-6 phone:p-5">
        <p class="text-sm">
          Remove “{{ saved.name }}” from your workspace? Referenced items must be removed from tasks first. Its grants and portrait are deleted.
        </p>
      </div>
      <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <UiButton :disabled="busy" @click="deleting = false">
          Cancel
        </UiButton>
        <UiButton variant="danger" :disabled="busy" @click="remove">
          Delete agent
        </UiButton>
      </footer>
    </Modal>
  </div>
</template>
