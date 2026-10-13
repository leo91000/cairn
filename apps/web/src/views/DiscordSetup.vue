<script setup lang="ts">
import type { DiscordConnection, Discovery, Label } from '../plugins'
import { twMerge } from 'tailwind-merge'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { api, notify, state } from '../api'
import AgentAvatar from '../components/AgentAvatar.vue'
import Icon from '../components/Icon.vue'
import PersonAvatar from '../components/PersonAvatar.vue'
import PluginMark from '../components/PluginMark.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  BookOpen,
  Check,
  ChevronRight,
  CircleCheck,
  ExternalLink,
  Hash,
  LoaderCircle,
  Lock,
  Plus,
  Power,
  RefreshCw,
  ShieldCheck,
  TriangleAlert,
  UserPlus,
  X,
} from '../icons'
import { eligibleAgent } from '../plugins'
import { iconButton } from '../ui'

const route = useRoute()
const router = useRouter()
const editing = computed(() => String(route.params.id ?? ''))
const portal = 'https://discord.com/developers/applications'
const guide = 'https://github.com/leo91000/cairn/blob/main/docs/PLUGINS.md#discord'
const steps = [
  ['Create the bot', 'Token from Discord'],
  ['Choose a channel', 'Where it listens'],
  ['Choose people', 'Who can use it'],
  ['Agent & limits', 'What it can do'],
] as const

const step = ref(0)
const busy = ref(false)
const checking = ref(false)
const error = ref('')
const token = ref('')
const discovery = ref<Discovery>()
const guildId = ref('')
const channelId = ref('')
const accessMode = ref<'requests' | 'allowlist'>('requests')
const messageContent = ref(false)
const people = ref<Label[]>([])
const addition = ref('')
const agentId = ref('')
const limit = ref(20)
const name = ref('')
const enabled = ref(true)

const guild = computed(() => discovery.value?.guilds.find(item => item.id === guildId.value))
const channel = computed(() => discovery.value?.channels?.find(item => item.id === channelId.value))
const agents = computed(() => [...state.agents].sort((a, b) => Number(eligibleAgent(b)) - Number(eligibleAgent(a))))
const agent = computed(() => state.agents.find(item => item.id === agentId.value))
const project = computed(() => state.projects.find(item => item.id === agent.value?.access.projects?.[0]))
const ready = computed(() => [
  !!discovery.value,
  !!guild.value && !!channel.value?.canReply,
  accessMode.value === 'requests' || people.value.length > 0,
  !!agent.value && eligibleAgent(agent.value) && name.value.trim().length > 0,
])

async function discover(extra: Record<string, unknown> = {}) {
  const body = token.value ? { token: token.value } : { connectionId: editing.value }
  return await api<Discovery>('/plugins/discord/discover', { method: 'POST', body: JSON.stringify({ ...body, ...extra }) })
}

async function verify() {
  if (!token.value && !editing.value)
    return
  checking.value = true
  error.value = ''
  try {
    discovery.value = await discover(guildId.value ? { guildId: guildId.value } : {})
  }
  catch (e) {
    discovery.value = undefined
    error.value = (e as Error).message
  }
  finally {
    checking.value = false
  }
}

let typing: ReturnType<typeof setTimeout> | undefined
watch(token, () => {
  discovery.value = undefined
  clearTimeout(typing)
  if (token.value.trim().length > 50 || (editing.value && !token.value))
    typing = setTimeout(verify, 400)
})

async function chooseGuild(id: string) {
  if (guildId.value !== id)
    channelId.value = ''
  guildId.value = id
  await verify()
}

async function addPerson() {
  const id = addition.value.trim()
  if (!/^\d{1,20}$/.test(id)) {
    error.value = 'Discord user IDs are numbers. Enable Developer Mode in Discord, then right-click a person → Copy User ID.'
    return
  }

  error.value = ''
  addition.value = ''
  if (people.value.some(person => person.id === id))
    return
  people.value.push({ id, name: '' })
  const found = await discover({ userIds: [id] }).catch(() => undefined)
  const label = found?.users?.find(user => user.id === id)
  if (label)
    people.value = people.value.map(person => person.id === id ? label : person)
}

function next() {
  if (step.value === 1 && !name.value)
    name.value = `#${channel.value?.name} on ${guild.value?.name}`.slice(0, 100)
  step.value++
}

async function save() {
  busy.value = true
  error.value = ''
  try {
    const body = {
      name: name.value.trim(),
      ...(token.value ? { token: token.value } : {}),
      settings: {
        guildId: guildId.value,
        guildName: guild.value?.name ?? '',
        channelId: channelId.value,
        channelName: channel.value?.name ?? '',
        botId: discovery.value?.bot.id ?? '',
        botName: discovery.value?.bot.name ?? '',
        messageContent: messageContent.value,
      },
      accessMode: accessMode.value,
      userIds: people.value.map(person => person.id),
      people: people.value,
      agentId: agentId.value,
      projectId: agent.value?.access.projects?.[0] ?? null,
      maxMessagesPerHour: limit.value,
      enabled: enabled.value,
    }
    await api(`/plugins/discord/connections${editing.value ? `/${editing.value}` : ''}`, {
      method: editing.value ? 'PUT' : 'POST',
      body: JSON.stringify(body),
    })
    token.value = ''
    notify(editing.value ? 'Bot updated' : `${body.name} is live`)
    await router.push('/plugins/discord')
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

onMounted(async () => {
  if (!editing.value)
    return
  try {
    const catalog = await api('/plugins')
    const existing = catalog.connections.find((item: DiscordConnection) => item.id === editing.value) as DiscordConnection | undefined
    if (!existing) {
      error.value = 'This bot no longer exists.'
      return
    }

    guildId.value = existing.settings.guildId
    channelId.value = existing.settings.channelId
    accessMode.value = existing.accessMode
    messageContent.value = existing.settings.messageContent === true
    people.value = existing.userIds.map(id => existing.people.find(person => person.id === id) ?? { id, name: '' })
    agentId.value = existing.agentId
    limit.value = existing.maxMessagesPerHour
    name.value = existing.name
    enabled.value = existing.enabled
    await verify()
  }
  catch (e) {
    error.value = (e as Error).message
  }
})
onBeforeUnmount(() => {
  clearTimeout(typing)
  token.value = ''
})

// Labels have unlayered global form styles; option cards override their direction and colour.
const option = 'flex flex-row! cursor-pointer items-center gap-3! text-ink! font-normal! rounded-[14px] border border-line bg-surface p-3.5 has-[:checked]:border-accent has-[:checked]:shadow-[0_0_0_3px_var(--color-soft)] has-[:disabled]:cursor-not-allowed has-[:disabled]:opacity-55 has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-accent'
const radio = 'size-4.5 shrink-0 accent-[var(--color-accent)]'
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <div class="flex items-center justify-between gap-4">
      <div class="flex items-center gap-3">
        <PluginMark plugin="discord" :size="34" />
        <div>
          <h1 class="text-[15px]! font-semibold! tracking-normal! leading-snug!">
            {{ editing ? 'Edit Discord bot' : 'Set up a Discord bot' }}
          </h1>
          <p class="text-2xs text-muted">
            Step {{ step + 1 }} of 4 · {{ steps[step][0] }}
          </p>
        </div>
      </div>
      <RouterLink to="/plugins/discord" class="inline-flex items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-semibold text-muted hover:bg-hover hover:text-ink">
        <Icon :name="X" :size="15" />Close
      </RouterLink>
    </div>

    <div class="mt-6 grid grid-cols-[230px_minmax(0,1fr)] items-start gap-7 tablet:grid-cols-1">
      <nav class="grid gap-1 tablet:hidden" aria-label="Setup steps">
        <button
          v-for="([title, detail], index) in steps"
          :key="title"
          class="flex items-center gap-3 rounded-xl border px-3 py-2.5 text-left text-[13px] font-semibold"
          :class="index === step ? 'border-line bg-surface text-ink shadow-arcade' : index < step ? 'border-transparent text-ink hover:bg-hover' : 'border-transparent text-muted'"
          :disabled="index > step"
          :aria-current="index === step ? 'step' : undefined"
          @click="step = index"
        >
          <span
            class="grid size-6 shrink-0 place-items-center rounded-full border-[1.5px] text-2xs"
            :class="index < step ? 'border-success bg-success text-white' : index === step ? 'border-accent bg-accent text-on-brand' : 'border-control'"
          ><Icon v-if="index < step" :name="Check" :size="13" /><template v-else>{{ index + 1 }}</template></span>
          <span>{{ title }}<small class="mt-px block text-2xs font-normal text-muted">{{ detail }}</small></span>
        </button>
      </nav>

      <form class="rounded-2xl border border-line bg-surface" @submit.prevent="step < 3 ? next() : save()">
        <div class="p-7 phone:p-5">
          <UiAlert v-if="error" class="mb-5">
            {{ error }}
          </UiAlert>

          <template v-if="step === 0">
            <h2 class="text-lg! font-bold">
              Create the bot in Discord
            </h2>
            <p class="mt-1.5 text-[13px] leading-relaxed text-muted">
              Discord bots belong to an application you own. Cairn never sees your Discord password.
            </p>
            <ol class="mt-5 grid gap-3">
              <li class="flex items-start gap-3">
                <span class="grid size-5.5 shrink-0 place-items-center rounded-[7px] bg-hover text-2xs font-bold text-muted">1</span>
                <span class="min-w-0 flex-1"><span class="block text-xs font-semibold">Create an application</span><span class="mt-0.5 block text-2xs text-muted">Name it as you want it to appear in your server, e.g. “Cairn”.</span></span>
                <a
                  :href="portal"
                  target="_blank"
                  rel="noopener"
                  class="inline-flex shrink-0 items-center gap-1.5 rounded-[9px] border border-line px-2.75 py-1.5 text-xs font-semibold text-ink hover:bg-hover"
                >Developer Portal<Icon :name="ExternalLink" :size="13" /></a>
              </li>
              <li class="flex items-start gap-3">
                <span class="grid size-5.5 shrink-0 place-items-center rounded-[7px] bg-hover text-2xs font-bold text-muted">2</span>
                <span><span class="block text-xs font-semibold">Open “Bot” and click “Reset Token”</span><span class="mt-0.5 block text-2xs text-muted">Copy the token. Discord shows it only once.</span></span>
              </li>
              <li class="flex items-start gap-3">
                <span class="grid size-5.5 shrink-0 place-items-center rounded-[7px] bg-hover text-2xs font-bold text-muted">3</span>
                <span class="text-xs font-semibold">Paste it here</span>
              </li>
            </ol>
            <label class="mt-5 grid gap-1.75 text-xs font-semibold">
              {{ editing ? 'New bot token (optional)' : 'Bot token' }}
              <span class="relative">
                <Icon :name="Lock" :size="15" class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted" />
                <input
                  v-model.trim="token"
                  type="password"
                  autocomplete="new-password"
                  spellcheck="false"
                  class="w-full pl-9!"
                  :class="discovery && token ? 'border-success!' : ''"
                  :placeholder="editing ? 'Saved · leave blank to keep' : 'Paste the token from the Bot page'"
                  @keydown.enter.prevent="verify"
                >
                <Icon
                  v-if="checking"
                  :name="LoaderCircle"
                  :size="17"
                  class="absolute right-3 top-1/2 -translate-y-1/2 animate-spin text-muted"
                />
                <Icon
                  v-else-if="discovery"
                  :name="CircleCheck"
                  :size="17"
                  class="absolute right-3 top-1/2 -translate-y-1/2 text-success"
                />
              </span>
            </label>
            <div v-if="discovery" class="mt-3 flex items-center gap-3 rounded-xl border border-line bg-inset px-3.5 py-3" role="status">
              <PersonAvatar :name="discovery.bot.name" :identity="discovery.bot.id" :size="36" />
              <span class="min-w-0 flex-1">
                <span class="block truncate text-xs font-semibold">{{ discovery.bot.name }}</span>
                <span class="block text-2xs text-muted">Bot account · in {{ discovery.guilds.length }} server{{ discovery.guilds.length === 1 ? '' : 's' }}</span>
              </span>
              <span class="inline-flex h-5.5 items-center gap-1 rounded-full bg-[light-dark(#dff3ee,#163a33)] px-2.25 text-2xs font-semibold text-success"><Icon :name="Check" :size="12" />Valid</span>
            </div>
            <UiButton
              v-else-if="token && !checking"
              size="small"
              class="mt-3"
              @click="verify"
            >
              Check token
            </UiButton>
            <p class="mt-3 flex items-center gap-1.5 text-2xs text-muted">
              <Icon :name="ShieldCheck" :size="12" />Stored encrypted in this installation’s vault. It isn’t shown again and never reaches agents.
            </p>
          </template>

          <template v-else-if="step === 1 && discovery">
            <h2 class="text-lg! font-bold">
              Where should it listen?
            </h2>
            <p class="mt-1.5 text-[13px] leading-relaxed text-muted">
              Invite the bot to a server, then pick the one channel where mentions start conversations.
            </p>
            <div class="mt-4.5 flex items-center gap-3 rounded-[14px] bg-soft px-3.5 py-3 text-xs text-accent phone:flex-wrap">
              <Icon :name="UserPlus" :size="18" />
              <span class="min-w-0 flex-1"><b>Invite {{ discovery.bot.name }} to a server.</b> The link only requests View Channel, Send Messages and Read Message History.</span>
              <a
                :href="discovery.inviteUrl"
                target="_blank"
                rel="noopener"
                class="inline-flex shrink-0 items-center gap-1.5 rounded-[9px] bg-[#5865F2] px-2.75 py-1.5 text-xs font-semibold text-white hover:bg-[#4752c4]"
              >Invite bot<Icon :name="ExternalLink" :size="13" /></a>
            </div>
            <div class="mt-4.5 grid grid-cols-2 gap-4.5 phone:grid-cols-1">
              <fieldset class="min-w-0">
                <legend class="mb-2 text-xs font-semibold">
                  Server
                </legend>
                <div class="grid gap-2.5">
                  <label v-for="item in discovery.guilds" :key="item.id" :class="option">
                    <input
                      type="radio"
                      name="guild"
                      :class="radio"
                      :value="item.id"
                      :checked="guildId === item.id"
                      @change="chooseGuild(item.id)"
                    >
                    <PersonAvatar
                      :name="item.name"
                      :identity="item.id"
                      :size="30"
                      class="rounded-[9px]!"
                    />
                    <span class="truncate text-xs font-semibold">{{ item.name }}</span>
                  </label>
                  <p v-if="!discovery.guilds.length" class="rounded-xl border border-dashed border-line p-4 text-2xs leading-relaxed text-muted">
                    The bot isn’t in any server yet. Use “Invite bot”, then refresh.
                  </p>
                </div>
                <button
                  type="button"
                  class="mt-2.5 inline-flex items-center gap-1.5 text-2xs text-muted hover:text-ink"
                  :disabled="checking"
                  @click="verify"
                >
                  <Icon :name="RefreshCw" :size="12" :class="checking ? 'animate-spin' : ''" />Not listed? Invite the bot, then refresh.
                </button>
              </fieldset>
              <fieldset v-if="guildId" class="min-w-0">
                <legend class="mb-2 text-xs font-semibold">
                  Channel
                </legend>
                <div class="grid gap-2.5">
                  <label v-for="item in discovery.channels ?? []" :key="item.id" :class="twMerge(option, 'py-2.75')">
                    <input
                      v-model="channelId"
                      type="radio"
                      name="channel"
                      :class="radio"
                      :value="item.id"
                      :disabled="!item.canReply"
                    >
                    <Icon :name="item.canRead ? Hash : Lock" :size="15" class="text-muted" />
                    <span class="min-w-0 flex-1 truncate text-xs font-semibold">{{ item.name }}</span>
                    <span v-if="item.canReply && item.canThread === false" class="text-2xs text-muted">No threads · replies in channel</span>
                    <span v-else-if="item.canReply" class="rounded-full bg-[light-dark(#dff3ee,#163a33)] px-2 py-px text-3xs font-semibold text-success">Can reply</span>
                    <span v-else class="text-2xs text-muted">{{ item.canRead ? 'Can’t send messages' : 'Bot can’t read' }}</span>
                  </label>
                  <p v-if="checking && !discovery.channels" class="text-2xs text-muted">
                    Loading channels…
                  </p>
                </div>
              </fieldset>
            </div>
          </template>

          <template v-else-if="step === 2">
            <h2 class="text-lg! font-bold">
              Who can talk to it?
            </h2>
            <p class="mt-1.5 text-[13px] leading-relaxed text-muted">
              Allowed people use the agent you choose next, with its access. Everyone else is ignored.
            </p>
            <fieldset class="mt-4.5 grid gap-2.5">
              <legend class="sr-only">
                Access
              </legend>
              <label :class="twMerge(option, 'items-start')">
                <input
                  v-model="accessMode"
                  type="radio"
                  name="access"
                  value="requests"
                  :class="twMerge(radio, 'mt-0.5')"
                >
                <span><span class="flex items-center gap-2 text-xs font-semibold">Approve people as they ask<span class="rounded-full bg-soft px-2 py-px text-3xs text-accent">Recommended</span></span>
                  <span class="mt-1 block text-2xs leading-relaxed text-muted">When someone new mentions the bot, it replies that they’re waiting for approval, and the request appears on the bot’s page in Cairn.</span></span>
              </label>
              <label :class="twMerge(option, 'items-start')">
                <input
                  v-model="accessMode"
                  type="radio"
                  name="access"
                  value="allowlist"
                  :class="twMerge(radio, 'mt-0.5')"
                >
                <span><span class="block text-xs font-semibold">Only people I add</span>
                  <span class="mt-1 block text-2xs leading-relaxed text-muted">Other mentions are ignored silently.</span></span>
              </label>
            </fieldset>
            <label :class="twMerge(option, 'mt-2.5 items-start')">
              <input v-model="messageContent" type="checkbox" :class="twMerge(radio, 'mt-0.5')">
              <span><span class="block text-xs font-semibold">Read replies without a mention</span>
                <span class="mt-1 block text-2xs leading-relaxed text-muted">Each conversation gets its own thread. With this on, allowed people can simply reply in the thread or to the bot. First turn on “Message Content Intent” on the Bot page of the Developer Portal, or Discord will refuse the connection.</span></span>
            </label>
            <h3 class="mt-5.5 text-xs! font-semibold">
              Allowed now
            </h3>
            <div class="mt-2 overflow-hidden rounded-[14px] border border-line">
              <div v-for="person in people" :key="person.id" class="flex items-center gap-3 border-b border-line px-3.5 py-2.5">
                <PersonAvatar :name="person.name || person.id" :identity="person.id" :size="30" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs font-semibold">{{ person.name || 'Discord user' }}</span>
                  <span class="block font-mono text-3xs text-muted">{{ person.id }}</span>
                </span>
                <button
                  type="button"
                  :class="iconButton"
                  :aria-label="`Remove ${person.name || person.id}`"
                  @click="people = people.filter(item => item.id !== person.id)"
                >
                  <Icon :name="X" :size="14" />
                </button>
              </div>
              <div class="flex items-center gap-2 px-2 py-1.5">
                <input
                  v-model="addition"
                  aria-label="Discord user ID"
                  inputmode="numeric"
                  placeholder="Add a Discord user ID"
                  class="min-w-0 flex-1 border-0! bg-transparent! shadow-none!"
                  @keydown.enter.prevent="addPerson"
                >
                <UiButton size="small" :disabled="!addition.trim()" @click="addPerson">
                  <Icon :name="Plus" :size="14" />Add
                </UiButton>
              </div>
            </div>
            <p class="mt-2 text-2xs leading-relaxed text-muted">
              {{ accessMode === 'requests' && !people.length ? `You can leave this empty: mention @${discovery?.bot.name ?? 'the bot'} yourself and approve your own request.` : 'To copy an ID, enable Developer Mode in Discord, then right-click a person → Copy User ID.' }}
            </p>
          </template>

          <template v-else-if="step === 3">
            <h2 class="text-lg! font-bold">
              Which agent answers?
            </h2>
            <p class="mt-1.5 text-[13px] leading-relaxed text-muted">
              Pick a dedicated agent. Only agents limited to one project at most, without installation access, are eligible.
            </p>
            <fieldset class="mt-4.5 grid gap-2.5">
              <legend class="sr-only">
                Agent
              </legend>
              <label v-for="item in agents" :key="item.id" :class="option">
                <input
                  v-model="agentId"
                  type="radio"
                  name="agent"
                  :class="radio"
                  :value="item.id"
                  :disabled="!eligibleAgent(item)"
                >
                <AgentAvatar :name="item.name" :identity="item.id" :size="32" />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs font-semibold">{{ item.name }}</span>
                  <span class="block truncate text-2xs text-muted">{{ item.provider === 'claude' ? 'Claude Code' : 'Codex' }} · {{ item.access.projects === null ? 'all projects' : state.projects.find(p => p.id === item.access.projects?.[0])?.name ?? 'no project' }}{{ item.access.installation ? ' · installation access' : '' }}</span>
                </span>
                <span v-if="!eligibleAgent(item)" class="inline-flex shrink-0 items-center gap-1 rounded-full bg-warning-surface px-2.25 py-0.5 text-2xs font-semibold text-warning"><Icon :name="TriangleAlert" :size="12" />Too broad</span>
              </label>
              <RouterLink to="/agents" class="inline-flex items-center gap-1.5 px-0.5 py-1 text-2xs font-semibold text-accent">
                <Icon :name="Plus" :size="13" />Create a dedicated agent
              </RouterLink>
            </fieldset>
            <div class="mt-5 grid grid-cols-2 gap-5 phone:grid-cols-1">
              <label class="grid gap-1.75 text-xs font-semibold">
                Name in Cairn
                <input v-model="name" maxlength="100" required>
              </label>
              <label class="grid gap-1.75 text-xs font-semibold">
                <span class="flex justify-between">Message limit<span class="font-normal"><b>{{ limit }}</b> <span class="text-muted">per hour</span></span></span>
                <input
                  v-model.number="limit"
                  type="range"
                  min="1"
                  max="100"
                  class="mt-2 w-full accent-[var(--color-accent)] border-0! p-0! shadow-none!"
                >
              </label>
            </div>
            <div v-if="agent && channel" class="mt-5 rounded-[14px] border border-line bg-inset px-4.5 py-4">
              <div class="text-xs font-semibold">
                Summary
              </div>
              <p class="mt-1.5 text-2xs leading-[1.7] text-muted">
                <b class="text-ink">{{ accessMode === 'requests' ? `${people.length ? `${people.length} ${people.length === 1 ? 'person' : 'people'}, and anyone you approve,` : 'People you approve'}` : `${people.length} ${people.length === 1 ? 'person' : 'people'}` }}</b>
                can mention <b class="text-ink">{{ discovery?.bot.name }}</b> in <b class="text-ink">#{{ channel.name }}</b> on {{ guild?.name }}.
                <b class="text-ink">{{ agent.name }}</b> answers{{ project ? `, working in ` : ' without project access' }}<b v-if="project" class="text-ink">{{ project.name }}</b>, up to {{ limit }} messages an hour.
              </p>
            </div>
          </template>
        </div>

        <footer class="flex items-center justify-between gap-3 border-t border-line px-7 py-4.5 phone:px-5">
          <a
            v-if="step === 0"
            :href="guide"
            target="_blank"
            rel="noopener"
            class="inline-flex items-center gap-1.5 text-xs font-semibold text-muted hover:text-ink"
          ><Icon :name="BookOpen" :size="14" />Detailed guide</a>
          <UiButton v-else @click="step--">
            Back
          </UiButton>
          <UiButton
            v-if="step < 3"
            type="submit"
            variant="primary"
            :disabled="!ready[step] || checking"
          >
            Continue<Icon :name="ChevronRight" :size="15" />
          </UiButton>
          <UiButton
            v-else
            type="submit"
            variant="primary"
            :disabled="!ready.every(Boolean) || busy"
          >
            <Icon :name="busy ? LoaderCircle : Power" :size="15" :class="busy ? 'animate-spin' : ''" />{{ editing ? 'Save changes' : 'Activate bot' }}
          </UiButton>
        </footer>
      </form>
    </div>
  </div>
</template>
