<script setup lang="ts">
import type { Catalog, DiscordConnection, Manifest } from '../plugins'
import { twMerge } from 'tailwind-merge'
import { onBeforeUnmount, onMounted, ref } from 'vue'
import {
  api,
  notify,
  relative,
  state,
} from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import PersonAvatar from '../components/PersonAvatar.vue'
import PluginMark from '../components/PluginMark.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  ArrowUpRight,
  Check,
  ChevronRight,
  Hash,
  MoreHorizontal,
  Pencil,
  Plus,
  ShieldCheck,
  Trash2,
  TriangleAlert,
  UserPlus,
  X,
  Zap,
} from '../icons'
import { connectionState, personName } from '../plugins'
import {
  buttonBase,
  buttonSizes,
  buttonVariants,
  iconButton,
} from '../ui'

const manifest = ref<Manifest | null>()
const connections = ref<DiscordConnection[]>([])
const onepassword = ref<{ name: string, agentIds: string[] }[]>([])
const loaded = ref(false)
const busy = ref(false)
const error = ref('')
const managing = ref<string>()
const removing = ref<DiscordConnection>()
let timer: ReturnType<typeof setInterval> | undefined
const primary = twMerge(buttonBase, buttonVariants.primary, buttonSizes.default)

async function load() {
  const [catalog, accounts] = await Promise.all([api<Catalog>('/plugins'), api('/onepassword')])
  manifest.value = catalog.packages.find(item => item.id === 'discord')?.manifest
  connections.value = catalog.connections.filter(item => item.pluginId === 'discord') as DiscordConnection[]
  onepassword.value = accounts
  loaded.value = true
}

async function action(operation: () => Promise<void>) {
  busy.value = true
  error.value = ''
  try {
    await operation()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

const agent = (connection: DiscordConnection) => state.agents.find(item => item.id === connection.agentId)
const project = (connection: DiscordConnection) => state.projects.find(item => item.id === connection.projectId)

function abilities(connection: DiscordConnection) {
  const access = agent(connection)?.access
  const secrets = onepassword.value.filter(account => account.agentIds.includes(connection.agentId))
  return [
    ...(connection.accessMode === 'everyone' ? [{ allowed: true, warn: true, text: 'Anyone in the channel can make the agent work (read-only)' }] : []),
    ...(connection.settings.everyMessage ? [{ allowed: true, text: 'Answer every message, staying silent when nothing is needed' }] : []),
    { allowed: true, text: `Start conversations with ${agent(connection)?.name ?? 'its agent'}` },
    project(connection)
      ? { allowed: true, text: `Read and change ${project(connection)!.name}` }
      : { allowed: false, text: 'Open any project' },
    { allowed: true, text: `Reply in #${connection.settings.channelName || connection.settings.channelId} and its threads only` },
    { allowed: true, text: 'Let allowed people answer its questions and stop it from Discord' },
    secrets.length
      ? { allowed: true, warn: true, text: `Use 1Password: ${secrets.map(account => account.name).join(', ')}` }
      : { allowed: false, text: 'Read 1Password secrets' },
    ...(access?.github ? [{ allowed: true, warn: true, text: 'Use GitHub as you' }] : []),
    { allowed: false, text: 'Manage the installation' },
    { allowed: false, text: 'See other conversations' },
  ]
}

async function test(connection: DiscordConnection) {
  await action(async () => {
    const result = await api(`/plugins/discord/connections/${connection.id}/test`, { method: 'POST' })
    notify(`Token works · signed in as ${result.name}`)
  })
}

async function toggle(connection: DiscordConnection) {
  await action(async () => {
    await api(`/plugins/discord/connections/${connection.id}/enabled`, { method: 'POST', body: JSON.stringify({ enabled: !connection.enabled }) })
    await load()
  })
}

async function person(connection: DiscordConnection, userId: string, operation: 'approve' | 'deny' | 'remove') {
  await action(async () => {
    await api(`/plugins/discord/connections/${connection.id}/people/${userId}/${operation}`, { method: 'POST' })
    await load()
  })
}

async function remove() {
  await action(async () => {
    await api(`/plugins/discord/connections/${removing.value!.id}`, { method: 'DELETE' })
    removing.value = undefined
    await load()
  })
}

function closeMenu(event: Event) {
  if ((event.target as HTMLElement).closest('button, a'))
    (event.currentTarget as HTMLDetailsElement).open = false
}

const tones = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
}
const activityTones = {
  working: ['bg-soft text-accent', 'Working'],
  replied: ['bg-[light-dark(#dff3ee,#163a33)] text-success', 'Replied'],
  failed: ['bg-coral-soft text-coral', 'Not delivered'],
  stopped: ['bg-hover text-muted', 'Stopped'],
} as const

onMounted(async () => {
  await action(load)
  timer = setInterval(() => {
    if (!busy.value && !managing.value)
      void load().catch(() => {})
  }, 5000)
})
onBeforeUnmount(() => clearInterval(timer))
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <nav class="flex items-center gap-1.5 text-xs font-medium text-muted" aria-label="Breadcrumb">
      <RouterLink to="/atelier" class="hover:text-ink phone:hidden">
        Atelier
      </RouterLink><Icon :name="ChevronRight" :size="12" class="phone:hidden" />
      <RouterLink to="/plugins" class="hover:text-ink">
        Plugins
      </RouterLink><Icon :name="ChevronRight" :size="12" /><span class="text-ink">Discord</span>
    </nav>
    <div class="mt-4.5 flex items-center justify-between gap-4">
      <div class="flex items-center gap-4">
        <PluginMark plugin="discord" :size="56" />
        <div>
          <h1 class="font-heading text-[26px]! font-extrabold leading-none! tracking-[-0.03em]!">
            Discord
          </h1>
          <p class="mt-2 flex flex-wrap items-center gap-2 text-2xs text-muted">
            <span class="rounded-full bg-soft px-2 py-0.5 font-semibold text-accent">Built-in</span>
            <span v-if="manifest">v{{ manifest.version }} · Luau sandbox · {{ manifest.capabilities.join(', ') }}</span>
          </p>
        </div>
      </div>
      <RouterLink v-if="connections.length" to="/plugins/discord/new" :class="twMerge(primary, 'phone:hidden')">
        <Icon :name="Plus" :size="16" />Add bot
      </RouterLink>
    </div>
    <UiAlert v-if="error" class="mt-5">
      {{ error }}
    </UiAlert>

    <section v-if="loaded && !connections.length" class="mt-6 rounded-2xl border border-line bg-surface p-8 phone:p-5">
      <h2 class="font-heading text-2xl! font-extrabold tracking-[-0.03em]!">
        Your agents, right in Discord.
      </h2>
      <p class="mt-2.5 max-w-140 text-sm leading-relaxed text-muted">
        Mention the bot in a channel and an agent picks up the request in its own thread: it shows what it’s doing, asks when it needs you, and replies with its answer and files. Every exchange is also a Cairn conversation.
      </p>
      <ul class="mt-4 grid gap-2 text-xs">
        <li v-for="line in ['Only people you approve can trigger it', 'Uses one dedicated agent, with its own limits', 'No public URL — the bot connects outward']" :key="line" class="flex items-center gap-2.5">
          <Icon :name="Check" :size="15" class="text-success" />{{ line }}
        </li>
      </ul>
      <div class="mt-6 flex items-center gap-3">
        <RouterLink to="/plugins/discord/new" :class="primary">
          Set up a bot
        </RouterLink><span class="text-2xs text-muted">About 3 minutes</span>
      </div>
    </section>

    <section
      v-for="connection in connections"
      :key="connection.id"
      class="mt-6"
      :aria-label="connection.name"
    >
      <div
        v-if="connection.requests.length"
        class="mb-4 flex items-center gap-3 rounded-2xl bg-coral-soft px-4 py-3 text-xs text-[light-dark(#8a3520,#ffb4a2)] phone:flex-wrap"
        role="status"
      >
        <Icon :name="UserPlus" :size="18" />
        <span class="min-w-0 flex-1"><b>{{ connection.requests.length }} {{ connection.requests.length > 1 ? 'people' : 'person' }} asked to use {{ connection.name }}.</b> They mentioned the bot but aren’t allowed yet.</span>
        <span class="flex -space-x-1 phone:hidden">
          <PersonAvatar
            v-for="request in connection.requests.slice(0, 3)"
            :key="request.id"
            :name="request.name"
            :identity="request.userId"
            :size="24"
          />
        </span>
        <UiButton size="small" class="bg-surface" @click="managing = connection.id">
          Review
        </UiButton>
      </div>

      <div class="grid grid-cols-[minmax(0,1fr)_300px] items-start gap-5 tablet:grid-cols-1">
        <article class="rounded-2xl border border-line bg-surface p-5.5 phone:p-4">
          <header class="flex items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-3">
              <span class="relative">
                <PersonAvatar :name="connection.settings.botName || connection.name" :identity="connection.settings.botId || connection.id" :size="42" />
                <span class="absolute -bottom-px -right-px size-3 rounded-full border-2 border-surface" :class="connectionState(connection).tone === 'ok' ? 'bg-[#23a55a]' : 'bg-control'" />
              </span>
              <div class="min-w-0">
                <h2 class="truncate text-[15px]! font-bold">
                  {{ connection.name }}
                </h2>
                <p class="mt-0.5 truncate text-2xs text-muted">
                  <template v-if="connection.settings.botName">
                    Signed in as <b class="text-ink">{{ connection.settings.botName }}</b>
                  </template><template v-else>
                    Discord bot
                  </template>
                </p>
              </div>
            </div>
            <div class="flex shrink-0 items-center gap-2">
              <span class="inline-flex h-5.5 items-center gap-1.5 rounded-full px-2.25 text-2xs font-semibold" :class="tones[connectionState(connection).tone]" role="status">
                <span class="size-1.75 rounded-full bg-current" />{{ connectionState(connection).label }}
              </span>
              <details class="task-action-menu relative" @click="closeMenu">
                <summary :class="twMerge(iconButton, 'icon-button')" :aria-label="`Actions for ${connection.name}`">
                  <Icon :name="MoreHorizontal" :size="18" />
                </summary><div>
                  <RouterLink :to="`/plugins/discord/${connection.id}/edit`">
                    Edit setup
                  </RouterLink><button :disabled="busy" @click="test(connection)">
                    Test connection
                  </button><button @click="removing = connection">
                    Remove bot
                  </button>
                </div>
              </details>
            </div>
          </header>
          <p v-if="connection.enabled && connection.status.state === 'needs_review'" class="mt-4 flex items-start gap-2 rounded-xl bg-coral-soft px-3.5 py-2.5 text-2xs leading-relaxed text-[light-dark(#8a3520,#ffb4a2)]">
            <Icon :name="TriangleAlert" :size="14" class="mt-px" />Discord or Cairn stopped this bot. The token may have been reset, or the agent’s access changed. Edit the setup to review it.
          </p>

          <div class="mt-5 grid grid-cols-3 gap-3 phone:grid-cols-1">
            <div class="flex min-w-0 flex-col gap-1.5 rounded-xl border border-line bg-inset px-4 py-3.5">
              <span class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">Listens in</span>
              <span class="flex items-center gap-1.5 truncate text-[13px] font-semibold"><Icon :name="Hash" :size="14" class="text-muted" />{{ connection.settings.channelName || connection.settings.channelId }}</span>
              <span class="truncate text-2xs text-muted">{{ connection.settings.guildName || 'Discord server' }}</span>
            </div>
            <button
              class="flex min-w-0 flex-col items-start gap-1.5 rounded-xl border border-line bg-inset px-4 py-3.5 text-left hover:border-control"
              :aria-label="`Manage people for ${connection.name}`"
              @click="managing = connection.id"
            >
              <span class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">Who can use it</span>
              <span v-if="connection.userIds.length" class="flex -space-x-1">
                <PersonAvatar
                  v-for="id in connection.userIds.slice(0, 5)"
                  :key="id"
                  :name="personName(connection, id)"
                  :identity="id"
                  :size="26"
                />
              </span>
              <span v-if="connection.accessMode === 'everyone'" class="text-2xs text-muted">Anyone in the channel · {{ connection.maxPerPersonPerHour ?? 5 }}/hour each</span>
              <span v-else class="text-2xs text-muted">{{ connection.userIds.length }} {{ connection.userIds.length === 1 ? 'person' : 'people' }}{{ connection.accessMode === 'requests' ? ' · approve as they ask' : '' }}</span>
            </button>
            <div class="flex min-w-0 flex-col gap-1.5 rounded-xl border border-line bg-inset px-4 py-3.5">
              <span class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">Answers as</span>
              <span class="flex min-w-0 items-center gap-2 text-[13px] font-semibold"><AgentAvatar :name="agent(connection)?.name ?? '?'" :identity="connection.agentId" :size="22" /><span class="truncate">{{ agent(connection)?.name ?? 'Missing agent' }}</span></span>
              <span class="truncate text-2xs text-muted">{{ project(connection)?.name ?? 'No project' }}</span>
            </div>
          </div>

          <div class="mt-4.5">
            <div class="flex justify-between text-2xs">
              <span class="text-muted">Usage this hour</span><span><b>{{ connection.usage.hour }}</b> <span class="text-muted">/ {{ connection.maxMessagesPerHour }} messages</span></span>
            </div>
            <div
              class="mt-2 h-1.5 overflow-hidden rounded-full bg-hover"
              role="meter"
              :aria-valuenow="connection.usage.hour"
              aria-valuemin="0"
              :aria-valuemax="connection.maxMessagesPerHour"
              aria-label="Messages this hour"
            >
              <span class="block h-full rounded-full bg-accent" :style="{ width: `${Math.min(100, connection.usage.hour / connection.maxMessagesPerHour * 100)}%` }" />
            </div>
          </div>

          <h3 class="mt-6.5 text-[13px]! font-semibold">
            Recent activity
          </h3>
          <p v-if="!connection.activity.length" class="mt-2 text-xs text-muted">
            Nothing yet. Mention <b class="text-ink">@{{ connection.settings.botName || 'the bot' }}</b> in #{{ connection.settings.channelName || connection.settings.channelId }} to try it.
          </p>
          <ul class="mt-1.5">
            <li v-for="item in connection.activity" :key="item.id" class="grid grid-cols-[28px_minmax(0,1fr)_auto] items-start gap-3 border-t border-line py-3 first:border-0">
              <PersonAvatar :name="item.name || personName(connection, item.userId)" :identity="item.userId" />
              <RouterLink :to="`/chats/${item.chatId}`" class="min-w-0 text-ink">
                <span class="block text-2xs text-muted"><b class="text-ink">{{ item.name || personName(connection, item.userId) }}</b> · {{ relative(item.createdAt) }}</span>
                <span class="mt-0.5 block truncate text-xs">“{{ item.preview }}”</span>
              </RouterLink>
              <span class="inline-flex h-5.5 items-center gap-1 rounded-full px-2.25 text-2xs font-semibold" :class="activityTones[item.status][0]">
                <Icon v-if="item.status === 'replied'" :name="Check" :size="12" />{{ activityTones[item.status][1] }}
              </span>
            </li>
          </ul>
        </article>

        <aside class="grid gap-3">
          <div class="rounded-2xl border border-line bg-surface p-4.5">
            <h3 class="flex items-center gap-2 text-[13px]! font-semibold">
              <Icon :name="ShieldCheck" :size="16" class="text-success" />What this bot can do
            </h3>
            <ul class="mt-2.5">
              <li
                v-for="line in abilities(connection)"
                :key="line.text"
                class="flex items-start gap-2.5 py-1.5 text-xs leading-normal"
                :class="line.allowed ? '' : 'text-muted'"
              >
                <Icon
                  :name="line.allowed ? (line.warn ? TriangleAlert : Check) : X"
                  :size="14"
                  class="mt-px"
                  :class="line.allowed ? (line.warn ? 'text-warning' : 'text-success') : 'text-subtle'"
                />{{ line.text }}
              </li>
            </ul>
            <RouterLink to="/agents" class="mt-2 inline-flex items-center gap-1 text-2xs font-semibold text-accent">
              Agent access <Icon :name="ArrowUpRight" :size="12" />
            </RouterLink>
          </div>
          <label class="flex flex-row! cursor-pointer items-center justify-between gap-3! rounded-2xl border border-line bg-surface px-4.5 py-4 text-ink!">
            <span>
              <span class="block text-[13px] font-semibold">Accept new messages</span>
              <span class="mt-0.5 block text-2xs font-normal text-muted">{{ connection.enabled ? 'Pause without losing the setup' : 'Paused · mentions are ignored' }}</span>
            </span>
            <input
              type="checkbox"
              role="switch"
              class="peer sr-only"
              :checked="connection.enabled"
              :disabled="busy"
              :aria-label="`Accept new messages for ${connection.name}`"
              @change="toggle(connection)"
            >
            <span class="relative h-5 w-8.5 shrink-0 rounded-full bg-control transition-colors peer-checked:bg-success peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-accent after:absolute after:left-0.5 after:top-0.5 after:size-4 after:rounded-full after:bg-white after:shadow-sm after:transition-transform peer-checked:after:translate-x-3.5" />
          </label>
          <div class="grid rounded-2xl border border-line bg-surface p-1.5 text-xs">
            <button class="flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-left hover:bg-hover" :disabled="busy" @click="test(connection)">
              <Icon :name="Zap" :size="15" class="text-muted" />Test connection
            </button>
            <RouterLink :to="`/plugins/discord/${connection.id}/edit`" class="flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-ink hover:bg-hover">
              <Icon :name="Pencil" :size="15" class="text-muted" />Edit setup
            </RouterLink>
            <button class="flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-left text-danger hover:bg-danger-surface" @click="removing = connection">
              <Icon :name="Trash2" :size="15" />Remove bot
            </button>
          </div>
        </aside>
      </div>
    </section>

    <RouterLink v-if="connections.length" to="/plugins/discord/new" :class="twMerge(primary, 'mt-6 hidden w-full phone:flex')">
      <Icon :name="Plus" :size="16" />Add bot
    </RouterLink>

    <template v-for="connection in connections" :key="connection.id">
      <Modal v-if="managing === connection.id" :title="`People · ${connection.name}`" @close="managing = undefined">
        <div class="grid gap-6 px-6.5 py-6 phone:p-5">
          <UiAlert v-if="error">
            {{ error }}
          </UiAlert>
          <section v-if="connection.requests.length">
            <h3 class="text-xs! font-semibold">
              Waiting for approval
            </h3>
            <ul class="mt-2 overflow-hidden rounded-xl border border-line">
              <li v-for="request in connection.requests" :key="request.id" class="flex items-center gap-3 border-t border-line px-3.5 py-2.5 first:border-0">
                <PersonAvatar :name="request.name" :identity="request.userId" :size="30" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs font-semibold">{{ request.name || request.userId }}</span>
                  <span class="block text-2xs text-muted">Asked {{ relative(request.updatedAt).toLowerCase() }}{{ request.mentions > 1 ? ` · ${request.mentions} mentions` : '' }}</span>
                </span>
                <UiButton
                  size="small"
                  :disabled="busy"
                  :aria-label="`Deny ${request.name}`"
                  @click="person(connection, request.userId, 'deny')"
                >
                  Deny
                </UiButton>
                <UiButton
                  size="small"
                  variant="primary"
                  :disabled="busy"
                  :aria-label="`Approve ${request.name}`"
                  @click="person(connection, request.userId, 'approve')"
                >
                  Approve
                </UiButton>
              </li>
            </ul>
          </section>
          <section>
            <h3 class="text-xs! font-semibold">
              Allowed
            </h3>
            <p v-if="!connection.userIds.length" class="mt-2 text-xs text-muted">
              Nobody yet. {{ connection.accessMode === 'requests' ? 'Mention the bot in Discord and approve your own request here.' : 'Add people from Edit setup.' }}
            </p>
            <ul v-else class="mt-2 overflow-hidden rounded-xl border border-line">
              <li v-for="id in connection.userIds" :key="id" class="flex items-center gap-3 border-t border-line px-3.5 py-2.5 first:border-0">
                <PersonAvatar :name="personName(connection, id)" :identity="id" :size="30" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs font-semibold">{{ personName(connection, id) }}</span>
                  <span class="block font-mono text-3xs text-muted">{{ id }}</span>
                </span>
                <button
                  :class="iconButton"
                  :disabled="busy"
                  :aria-label="`Remove ${personName(connection, id)}`"
                  @click="person(connection, id, 'remove')"
                >
                  <Icon :name="X" :size="15" />
                </button>
              </li>
            </ul>
          </section>
        </div>
      </Modal>
    </template>

    <Modal v-if="removing" :title="`Remove ${removing.name}?`" @close="removing = undefined">
      <div class="px-6.5 py-6 phone:p-5">
        <p class="text-sm text-muted">
          The bot stops receiving work and delivering replies, and its token is deleted. Conversations stay in Cairn. You can also remove the bot from your server in Discord.
        </p>
      </div>
      <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5 phone:flex-wrap phone:px-5 phone:py-4">
        <UiButton :disabled="busy" @click="removing = undefined">
          Cancel
        </UiButton>
        <UiButton variant="danger" :disabled="busy" @click="remove">
          <Icon :name="Trash2" :size="15" />Remove bot
        </UiButton>
      </footer>
    </Modal>
  </div>
</template>
