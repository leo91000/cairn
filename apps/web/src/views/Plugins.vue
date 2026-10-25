<script setup lang="ts">
import type { IconName } from '../icons'
import type { PluginCardEntry, PluginCards } from '../plugin-catalogue'
import {
  computed,
  onMounted,
  onUnmounted,
  ref,
} from 'vue'
import { api, notify } from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import PluginMark from '../components/PluginMark.vue'
import PluginReview from '../components/PluginReview.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  ArrowLeft,
  ArrowUpRight,
  BookOpen,
  ChevronRight,
  KeyRound,
  LayoutPanelTop,
  Plus,
  Puzzle,
  ShieldCheck,
  Terminal,
  Upload,
  Users,
  Wrench,
} from '../icons'
import { pluginCards } from '../plugin-catalogue'
import { effortLabels } from '../plugin-setup'
import {
  capabilityLabels,
  connectionState,
  instanceNoun,
} from '../plugins'

const catalog = ref<PluginCards>({ items: [], capabilities: [], packageReads: false })
const controller = new AbortController()
onUnmounted(() => controller.abort())
const loaded = ref(false)
const error = ref('')
const reviewing = ref<PluginCardEntry>()
const installing = ref(false)
const files = ref<File[]>([])
const busy = ref(false)
const guide = 'https://github.com/leo91000/cairn/blob/main/docs/PLUGINS.md'

async function load() {
  catalog.value = await pluginCards(controller.signal)
  loaded.value = true
  error.value = ''
}

async function refresh() {
  try {
    await load()
  }
  catch (e) {
    if (!controller.signal.aborted)
      error.value = (e as Error).message
  }
}

onMounted(refresh)

interface Card {
  entry: PluginCardEntry
  to?: string
  capabilities: [IconName, string][]
  status: { label: string, tone: 'ok' | 'muted' | 'coral' | 'warn' }
  meta: string
  requests: number
  action: 'Set up' | 'Manage' | 'Review'
  /** What connecting takes, until the first instance exists. */
  effort?: { label: string, tone: 'warn' | 'accent' }
}

const tones = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
  warn: 'bg-warning-surface text-warning',
}

function card(entry: PluginCardEntry): Card {
  const { presentation, summary } = entry
  const capabilities: [IconName, string][] = [
    ...(presentation?.capabilities ?? []).map(name => capabilityLabels[name] ?? [ShieldCheck, name] as [IconName, string]),
    ...(presentation?.contributions.commands ? [[Terminal, 'CLI commands'] as [IconName, string]] : []),
    ...(presentation?.contributions.tools ? [[Wrench, 'Agent tools'] as [IconName, string]] : []),
    ...(presentation?.agentAccess ? [[KeyRound, 'Per-agent grants'] as [IconName, string]] : []),
    ...(presentation?.contributions.view ? [[LayoutPanelTop, 'Dashboard'] as [IconName, string]] : []),
  ]
  if (entry.status === 'needs_approval' || entry.status === 'invalid') {
    return {
      entry,
      capabilities,
      status: entry.status === 'invalid' ? { label: 'Invalid', tone: 'coral' } : { label: 'Needs approval', tone: 'warn' },
      meta: presentation ? `Version ${presentation.version}` : 'Check the package files',
      requests: 0,
      action: 'Review',
    }
  }

  const effort = presentation?.setup?.effort
  const online = (summary.states.connected ?? 0) + (summary.states.ready ?? 0)
  const noun = instanceNoun(presentation, summary.instances).toLowerCase()
  const agents = summary.authorizedAgents
  const granted = presentation?.agentAccess
    ? agents === null ? ' · Agent access details unavailable' : ` · ${agents} agent${agents === 1 ? '' : 's'} with saved access`
    : ''
  const state = ['needs_review', 'connecting', 'disconnected', 'paused'].find(state => summary.states[state])
    ?? Object.keys(summary.states).find(state => summary.states[state]) ?? 'disconnected'
  return {
    entry,
    to: `/plugins/${entry.id}`,
    capabilities: presentation?.delegates ? [...capabilities, [Users, 'External people']] : capabilities,
    status: entry.status === 'update'
      ? { label: 'Update to review', tone: 'warn' }
      : !summary.instances
          ? { label: 'Not set up', tone: 'muted' }
          : online
            ? { label: `${online} ${presentation?.needsWorker ? 'online' : 'ready'}`, tone: 'ok' }
            : connectionState({ enabled: state !== 'paused', status: { state } }),
    meta: summary.instances ? `${summary.instances} ${noun}${granted}` : `Version ${entry.active}`,
    requests: summary.pendingRequests,
    action: summary.instances ? 'Manage' : 'Set up',
    effort: !summary.instances && effort ? { label: effortLabels[effort], tone: effort === 'app' ? 'warn' : 'accent' } : undefined,
  }
}

const cards = computed(() => catalog.value.items.map(card))

const soon = [
  { id: 'slack', name: 'Slack', detail: 'Bot in channels and DMs' },
  { id: 'telegram', name: 'Telegram', detail: 'Chat with an agent on the go' },
  { id: 'linear', name: 'Linear', detail: 'Turn issues into missions' },
]

/** A chosen file's path inside the package: its folder's own name is dropped. */
function packagePath(file: File) {
  const path = file.webkitRelativePath || file.name
  return path.includes('/') ? path.slice(path.indexOf('/') + 1) : path
}

/** Reads a package (JSON/TOML manifest, main.luau and its modules) and installs it inactive. */
async function install() {
  busy.value = true
  error.value = ''
  try {
    const manifests = files.value.filter(file => ['manifest.json', 'manifest.toml'].includes(packagePath(file)))
    if (manifests.length !== 1)
      throw new Error('Choose exactly one manifest.json or manifest.toml.')
    const manifest = manifests[0]
    const source = files.value.find(file => packagePath(file) === 'main.luau')
    if (!manifest || !source)
      throw new Error('Choose the package’s manifest.json or manifest.toml and main.luau, or its folder.')
    const modules: Record<string, string> = {}
    for (const file of files.value) {
      const path = packagePath(file)
      if (path.endsWith('.luau') && path !== 'main.luau')
        modules[path] = await file.text()
    }

    const installed = await api<{ manifest: { id: string, name: string } }>('/plugins/packages', {
      method: 'POST',
      body: JSON.stringify({
        manifest: await manifest.text(),
        manifestFormat: packagePath(manifest).endsWith('.toml') ? 'toml' : 'json',
        source: await source.text(),
        modules,
      }),
    })
    installing.value = false
    files.value = []
    await load()
    notify(`${installed.manifest.name} installed · review it to activate`)
    reviewing.value = catalog.value.items.find(entry => entry.id === installed.manifest.id)
  }
  catch (e) {
    error.value = e instanceof SyntaxError ? 'manifest.json is not valid JSON.' : (e as Error).message
  }
  finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <RouterLink to="/atelier" class="inline-flex items-center gap-1.5 text-xs font-medium text-muted hover:text-ink">
      <Icon :name="ArrowLeft" :size="14" />Atelier
    </RouterLink>
    <div class="mt-3 flex items-end justify-between gap-4 phone:flex-wrap">
      <div>
        <h1 class="font-heading text-[28px]! font-extrabold leading-none! tracking-[-0.03em]!">
          Plugins
        </h1>
        <p class="mt-2.5 max-w-140 text-[13px] leading-relaxed text-muted">
          Give your agents new abilities and new places to work. Plugins run in an isolated Luau sandbox and only get the access you approve.
        </p>
      </div>
      <div class="flex shrink-0 items-center gap-1">
        <a
          :href="guide"
          target="_blank"
          rel="noopener"
          class="inline-flex items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-semibold text-muted hover:bg-hover hover:text-ink phone:hidden"
        >
          <Icon :name="BookOpen" :size="14" />Build a plugin
        </a>
        <UiButton @click="installing = true; error = ''">
          <Icon :name="Upload" :size="15" />Install package
        </UiButton>
      </div>
    </div>
    <UiAlert v-if="error && !installing" class="mt-4">
      {{ error }}
    </UiAlert>

    <section class="mt-9" aria-labelledby="plugins-installed">
      <h2 id="plugins-installed" class="eyebrow mb-3 flex items-center gap-2 text-base!">
        Installed <span v-if="loaded" class="rounded-full bg-hover px-1.75 py-px text-3xs tracking-normal text-muted">{{ cards.length }}</span>
      </h2>
      <div class="grid grid-cols-2 gap-3.5 tablet:grid-cols-1">
        <component
          :is="item.to ? 'RouterLink' : 'button'"
          v-for="item in cards"
          :key="item.entry.id"
          :to="item.to"
          :aria-label="`${item.entry.presentation?.name ?? item.entry.id} · ${item.action}`"
          class="lift flex flex-col gap-4 rounded-2xl border border-line bg-surface p-5 text-left text-ink"
          @click="item.to ? undefined : reviewing = item.entry"
        >
          <span class="flex items-start gap-3">
            <PluginMark :plugin="item.entry.id" :brand="item.entry.presentation?.brand" :size="46" />
            <span class="min-w-0 flex-1">
              <span class="flex items-center gap-2">
                <span class="truncate text-[15px] font-bold tracking-[-0.01em]">{{ item.entry.presentation?.name ?? item.entry.id }}</span>
                <span v-if="item.entry.builtin" class="rounded-full bg-soft px-2 py-0.5 text-3xs font-semibold text-accent">Built-in</span>
                <span v-else class="rounded-full bg-hover px-2 py-0.5 text-3xs font-semibold text-muted">Installed</span>
              </span>
              <span class="mt-1 block text-xs leading-normal text-muted">{{ item.entry.presentation?.description || 'No description.' }}</span>
            </span>
          </span>
          <span class="flex flex-wrap gap-1.5">
            <span v-for="[icon, label] in item.capabilities" :key="label" class="inline-flex h-6 items-center gap-1.25 rounded-[7px] border border-line bg-inset px-2.25 text-2xs font-medium text-muted">
              <Icon :name="icon" :size="12" />{{ label }}
            </span>
          </span>
          <span class="mt-auto flex items-center justify-between gap-3 border-t border-line pt-4">
            <span class="flex min-w-0 flex-wrap items-center gap-2.5">
              <span v-if="loaded" class="inline-flex h-5.5 items-center gap-1.5 rounded-full px-2.25 text-2xs font-semibold" :class="tones[item.status.tone]">
                <span class="size-1.75 rounded-full bg-current" />{{ item.status.label }}
              </span>
              <span v-if="item.effort" class="inline-flex h-5.5 items-center rounded-full px-2.25 text-2xs font-semibold" :class="item.effort.tone === 'warn' ? 'bg-warning-surface text-warning' : 'bg-soft text-accent'">
                {{ item.effort.label }}
              </span>
              <span v-if="item.requests" class="inline-flex h-5.5 items-center rounded-full bg-coral-soft px-2.25 text-2xs font-semibold text-coral">
                {{ item.requests }} request{{ item.requests > 1 ? 's' : '' }}
              </span>
              <span class="text-2xs text-muted">{{ item.meta }}</span>
            </span>
            <span
              class="inline-flex h-7.5 shrink-0 items-center gap-1 rounded-[9px] border px-2.75 text-xs font-semibold"
              :class="item.action === 'Manage' ? 'border-line' : 'border-transparent bg-brand text-on-brand'"
            >
              {{ item.action }}<Icon :name="ChevronRight" :size="14" />
            </span>
          </span>
        </component>
      </div>
    </section>

    <section class="mt-9" aria-labelledby="plugins-soon">
      <h2 id="plugins-soon" class="eyebrow mb-3 text-base!">
        Coming next
      </h2>
      <div class="grid grid-cols-3 gap-3 tablet:grid-cols-1">
        <div v-for="item in soon" :key="item.id" class="flex items-center gap-3 rounded-2xl border border-dashed border-line p-4">
          <PluginMark :plugin="item.id" :size="38" />
          <span class="min-w-0">
            <span class="flex items-center gap-2 text-[13px] font-semibold">{{ item.name }}<span class="rounded-full bg-hover px-2 py-px text-3xs font-semibold text-muted">Soon</span></span>
            <span class="mt-0.5 block text-2xs text-muted">{{ item.detail }}</span>
          </span>
        </div>
      </div>
    </section>

    <a
      :href="guide"
      target="_blank"
      rel="noopener"
      class="lift mt-7 flex items-center gap-4 rounded-2xl border border-line bg-inset px-5 py-4.5 text-ink"
    >
      <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-soft text-accent"><Icon :name="Puzzle" :size="20" /></span>
      <span class="min-w-0 flex-1">
        <span class="block text-[13px] font-semibold">Write your own</span>
        <span class="mt-0.5 block text-2xs text-muted">A plugin is a manifest.json or manifest.toml and a main.luau: its connection, forms, panels and commands. Start from the Discord or 1Password source.</span>
      </span>
      <span class="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-accent phone:hidden">Read the guide<Icon :name="ArrowUpRight" :size="13" /></span>
    </a>

    <PluginReview
      v-if="reviewing"
      :id="reviewing.id"
      :package-name="reviewing.presentation?.name"
      :capabilities="catalog.capabilities"
      @close="reviewing = undefined"
      @changed="refresh"
    />

    <Modal v-if="installing" title="Install a plugin package" @close="installing = false">
      <form class="grid gap-5 px-6.5 py-6 phone:p-5" @submit.prevent="install">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <p class="text-xs leading-relaxed text-muted">
          Choose the package’s <code>manifest.json</code> or <code>manifest.toml</code>, <code>main.luau</code> and its other <code>.luau</code> files, or its whole folder. It is saved on this installation but stays inactive until you review and approve what it can do.
        </p>
        <label class="text-xs font-semibold">
          Package files
          <input
            type="file"
            multiple
            accept=".json,.toml,.luau"
            aria-label="Package files"
            @change="files = [...(($event.target as HTMLInputElement).files ?? [])]"
          >
        </label>
        <label class="text-xs font-semibold">
          Or the package folder
          <input
            type="file"
            webkitdirectory
            aria-label="Package folder"
            @change="files = [...(($event.target as HTMLInputElement).files ?? [])]"
          >
        </label>
        <ul v-if="files.length" class="grid gap-1 text-2xs text-muted">
          <li v-for="file in files" :key="packagePath(file)">
            {{ packagePath(file) }} · {{ Math.ceil(file.size / 1024) }} KB
          </li>
        </ul>
        <div class="flex justify-end gap-2.5">
          <UiButton :disabled="busy" @click="installing = false">
            Cancel
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="busy || files.length < 2">
            <Icon :name="Plus" :size="15" />Install
          </UiButton>
        </div>
      </form>
    </Modal>
  </div>
</template>
