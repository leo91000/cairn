<script setup lang="ts">
import type { IconName } from '../icons'
import type { Catalog, PluginEntry } from '../plugins'
import { computed, onMounted, ref } from 'vue'
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
  Plus,
  Puzzle,
  ShieldCheck,
  Upload,
  Users,
} from '../icons'
import {
  capabilityLabels,
  connectionState,
  contributions,
  instanceNoun,
  needsWorker,
} from '../plugins'

const catalog = ref<Catalog>({ packages: [], capabilities: [], connections: [] })
const loaded = ref(false)
const error = ref('')
const reviewing = ref<PluginEntry>()
const installing = ref(false)
const files = ref<File[]>([])
const busy = ref(false)
const guide = 'https://github.com/leo91000/cairn/blob/main/docs/PLUGINS.md'

async function load() {
  catalog.value = await api<Catalog>('/plugins')
  loaded.value = true
}

onMounted(async () => {
  try {
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
  }
})

interface Card {
  entry: PluginEntry
  to?: string
  capabilities: [IconName, string][]
  status: { label: string, tone: 'ok' | 'muted' | 'coral' | 'warn' }
  meta: string
  requests: number
  action: 'Set up' | 'Manage' | 'Review'
}

const tones = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
  warn: 'bg-warning-surface text-warning',
}

function card(entry: PluginEntry): Card {
  const connections = catalog.value.connections.filter(item => item.pluginId === entry.id)
  const requests = connections.reduce((count, connection) => count + connection.requests.length, 0)
  const capabilities = [
    ...(entry.manifest?.capabilities ?? []).map(name => capabilityLabels[name] ?? [ShieldCheck, name] as [IconName, string]),
    ...contributions(entry.manifest),
  ]

  if (entry.status === 'needs_approval' || entry.status === 'invalid') {
    return {
      entry,
      capabilities,
      status: entry.status === 'invalid' ? { label: 'Invalid', tone: 'coral' } : { label: 'Needs approval', tone: 'warn' },
      meta: entry.manifest ? `Version ${entry.versions.at(-1)?.version}` : 'Check the package files',
      requests: 0,
      action: 'Review',
    }
  }

  const online = connections.filter(connection => connectionState(connection).tone === 'ok').length
  const noun = instanceNoun(entry.manifest, connections.length).toLowerCase()
  const agents = new Set(connections.flatMap(connection => connection.agentIds ?? [])).size
  const granted = entry.manifest?.agentAccess ? ` · ${agents} agent${agents === 1 ? '' : 's'} authorized` : ''
  return {
    entry,
    to: `/plugins/${entry.id}`,
    capabilities: entry.id === 'discord' ? [...capabilities, [Users, 'External people']] : capabilities,
    status: entry.status === 'update'
      ? { label: 'Update to review', tone: 'warn' }
      : !connections.length
          ? { label: 'Not set up', tone: 'muted' }
          : online
            ? { label: `${online} ${needsWorker(entry.manifest) ? 'online' : 'ready'}`, tone: 'ok' }
            : connectionState(connections[0]),
    meta: connections.length ? `${connections.length} ${noun}${granted}` : `Version ${entry.active}`,
    requests,
    action: connections.length ? 'Manage' : 'Set up',
  }
}

const cards = computed(() => catalog.value.packages.map(card))

const soon = [
  { id: 'slack', name: 'Slack', detail: 'Bot in channels and DMs' },
  { id: 'telegram', name: 'Telegram', detail: 'Chat with an agent on the go' },
  { id: 'linear', name: 'Linear', detail: 'Turn issues into missions' },
]

/** Reads a package folder (manifest.json and main.luau) and installs it inactive. */
async function install() {
  busy.value = true
  error.value = ''
  try {
    const manifest = files.value.find(file => file.name === 'manifest.json')
    const source = files.value.find(file => file.name === 'main.luau')
    if (!manifest || !source)
      throw new Error('Choose the package’s manifest.json and main.luau.')
    const installed = await api<{ manifest: { id: string, name: string } }>('/plugins/packages', {
      method: 'POST',
      body: JSON.stringify({ manifest: JSON.parse(await manifest.text()), source: await source.text() }),
    })
    installing.value = false
    files.value = []
    await load()
    notify(`${installed.manifest.name} installed · review it to activate`)
    reviewing.value = catalog.value.packages.find(entry => entry.id === installed.manifest.id)
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
          :aria-label="`${item.entry.manifest?.name ?? item.entry.id} · ${item.action}`"
          class="lift flex flex-col gap-4 rounded-2xl border border-line bg-surface p-5 text-left text-ink"
          @click="item.to ? undefined : reviewing = item.entry"
        >
          <span class="flex items-start gap-3">
            <PluginMark :plugin="item.entry.id" :brand="item.entry.manifest?.brand" :size="46" />
            <span class="min-w-0 flex-1">
              <span class="flex items-center gap-2">
                <span class="truncate text-[15px] font-bold tracking-[-0.01em]">{{ item.entry.manifest?.name ?? item.entry.id }}</span>
                <span v-if="item.entry.builtin" class="rounded-full bg-soft px-2 py-0.5 text-3xs font-semibold text-accent">Built-in</span>
                <span v-else class="rounded-full bg-hover px-2 py-0.5 text-3xs font-semibold text-muted">Installed</span>
              </span>
              <span class="mt-1 block text-xs leading-normal text-muted">{{ item.entry.manifest?.description || 'No description.' }}</span>
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
        <span class="mt-0.5 block text-2xs text-muted">A plugin is a manifest.json and a main.luau. Start from the Discord plugin source.</span>
      </span>
      <span class="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-accent phone:hidden">Read the guide<Icon :name="ArrowUpRight" :size="13" /></span>
    </a>

    <PluginReview
      v-if="reviewing"
      :entry="reviewing"
      :capabilities="catalog.capabilities"
      @close="reviewing = undefined"
      @changed="load"
    />

    <Modal v-if="installing" title="Install a plugin package" @close="installing = false">
      <form class="grid gap-5 px-6.5 py-6 phone:p-5" @submit.prevent="install">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <p class="text-xs leading-relaxed text-muted">
          Choose the package’s <code>manifest.json</code> and <code>main.luau</code>. It is saved on this installation but stays inactive until you review and approve what it can do.
        </p>
        <label class="text-xs font-semibold">
          Package files
          <input
            type="file"
            multiple
            accept=".json,.luau"
            aria-label="Package files"
            @change="files = [...(($event.target as HTMLInputElement).files ?? [])]"
          >
        </label>
        <ul v-if="files.length" class="grid gap-1 text-2xs text-muted">
          <li v-for="file in files" :key="file.name">
            {{ file.name }} · {{ Math.ceil(file.size / 1024) }} KB
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
