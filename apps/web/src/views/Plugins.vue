<script setup lang="ts">
import type { IconName } from '../icons'
import type { DiscordConnection, PluginPackage } from '../plugins'
import { computed, onMounted, ref } from 'vue'
import { api } from '../api'
import Icon from '../components/Icon.vue'
import PluginMark from '../components/PluginMark.vue'
import UiAlert from '../components/UiAlert.vue'
import {
  ArrowLeft,
  ArrowUpRight,
  BookOpen,
  Bot,
  ChevronRight,
  KeyRound,
  MessageSquare,
  Puzzle,
  ShieldCheck,
  Terminal,
  Users,
} from '../icons'
import { connectionState } from '../plugins'

const packages = ref<PluginPackage[]>([])
const connections = ref<DiscordConnection[]>([])
const accounts = ref<{ enabled: boolean, agentIds: string[] }[]>([])
const loaded = ref(false)
const error = ref('')
const guide = 'https://github.com/leo91000/cairn/blob/main/docs/PLUGINS.md'

onMounted(async () => {
  try {
    const [catalog, onepassword] = await Promise.all([api('/plugins'), api('/onepassword')])
    packages.value = catalog.packages
    connections.value = catalog.connections
    accounts.value = onepassword
    loaded.value = true
  }
  catch (e) {
    error.value = (e as Error).message
  }
})

interface Card {
  id: string
  to: string
  capabilities: [IconName, string][]
  status: { label: string, tone: 'ok' | 'muted' | 'coral' }
  meta: string
  requests: number
  setup: boolean
}

const cards = computed<Card[]>(() => {
  const agents = new Set(accounts.value.flatMap(account => account.agentIds))
  const online = connections.value.filter(connection => connectionState(connection).tone === 'ok').length
  const requests = connections.value.reduce((count, connection) => count + connection.requests.length, 0)
  const bots = connections.value.length
  return [
    {
      id: 'onepassword',
      to: '/connections',
      capabilities: [[Terminal, 'CLI tool'], [KeyRound, 'Secrets'], [ShieldCheck, 'Per-agent grants']],
      status: accounts.value.length
        ? { label: `${accounts.value.length} account${accounts.value.length > 1 ? 's' : ''}`, tone: 'ok' }
        : { label: 'Not set up', tone: 'muted' },
      meta: accounts.value.length ? `${agents.size} agent${agents.size === 1 ? '' : 's'} authorized` : 'Add a service account',
      requests: 0,
      setup: !accounts.value.length,
    },
    {
      id: 'discord',
      to: '/plugins/discord',
      capabilities: [[Bot, 'Background bot'], [MessageSquare, 'Conversations'], [Users, 'External people']],
      status: !bots
        ? { label: 'Not set up', tone: 'muted' }
        : online
          ? { label: `${online} online`, tone: 'ok' }
          : connectionState(connections.value[0]),
      meta: bots ? `${bots} bot${bots > 1 ? 's' : ''}` : 'About 3 minutes to set up',
      requests,
      setup: !bots,
    },
  ]
})

const soon = [
  { id: 'slack', name: 'Slack', detail: 'Bot in channels and DMs' },
  { id: 'telegram', name: 'Telegram', detail: 'Chat with an agent on the go' },
  { id: 'linear', name: 'Linear', detail: 'Turn issues into missions' },
]
const tones = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
}
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <RouterLink to="/atelier" class="inline-flex items-center gap-1.5 text-xs font-medium text-muted hover:text-ink">
      <Icon :name="ArrowLeft" :size="14" />Atelier
    </RouterLink>
    <div class="mt-3 flex items-end justify-between gap-4">
      <div>
        <h1 class="font-heading text-[28px]! font-extrabold leading-none! tracking-[-0.03em]!">
          Plugins
        </h1>
        <p class="mt-2.5 max-w-140 text-[13px] leading-relaxed text-muted">
          Give your agents new abilities and new places to work. Plugins run in an isolated Luau sandbox and only get the access you grant.
        </p>
      </div>
      <a
        :href="guide"
        target="_blank"
        rel="noopener"
        class="inline-flex shrink-0 items-center gap-1.5 rounded-lg px-3 py-2 text-xs font-semibold text-muted hover:bg-hover hover:text-ink phone:hidden"
      >
        <Icon :name="BookOpen" :size="14" />Build a plugin
      </a>
    </div>
    <UiAlert v-if="error" class="mt-4">
      {{ error }}
    </UiAlert>

    <section class="mt-9" aria-labelledby="plugins-installed">
      <h2 id="plugins-installed" class="eyebrow mb-3 flex items-center gap-2 text-base!">
        Installed <span v-if="loaded" class="rounded-full bg-hover px-1.75 py-px text-3xs tracking-normal text-muted">{{ packages.length }}</span>
      </h2>
      <div class="grid grid-cols-2 gap-3.5 tablet:grid-cols-1">
        <RouterLink
          v-for="card in cards"
          :key="card.id"
          :to="card.to"
          :aria-label="`${packages.find(item => item.id === card.id)?.name ?? card.id} · ${card.setup ? 'Set up' : 'Manage'}`"
          class="lift flex flex-col gap-4 rounded-2xl border border-line bg-surface p-5 text-ink"
        >
          <span class="flex items-start gap-3">
            <PluginMark :plugin="card.id" :size="46" />
            <span class="min-w-0 flex-1">
              <span class="flex items-center gap-2">
                <span class="text-[15px] font-bold tracking-[-0.01em]">{{ packages.find(item => item.id === card.id)?.name }}</span>
                <span class="rounded-full bg-soft px-2 py-0.5 text-3xs font-semibold text-accent">Built-in</span>
              </span>
              <span class="mt-1 block text-xs leading-normal text-muted">{{ packages.find(item => item.id === card.id)?.description }}</span>
            </span>
          </span>
          <span class="flex flex-wrap gap-1.5">
            <span v-for="[icon, label] in card.capabilities" :key="label" class="inline-flex h-6 items-center gap-1.25 rounded-[7px] border border-line bg-inset px-2.25 text-2xs font-medium text-muted">
              <Icon :name="icon" :size="12" />{{ label }}
            </span>
          </span>
          <span class="mt-auto flex items-center justify-between gap-3 border-t border-line pt-4">
            <span class="flex min-w-0 flex-wrap items-center gap-2.5">
              <span v-if="loaded" class="inline-flex h-5.5 items-center gap-1.5 rounded-full px-2.25 text-2xs font-semibold" :class="tones[card.status.tone]">
                <span class="size-1.75 rounded-full bg-current" />{{ card.status.label }}
              </span>
              <span v-if="card.requests" class="inline-flex h-5.5 items-center rounded-full bg-coral-soft px-2.25 text-2xs font-semibold text-coral">
                {{ card.requests }} request{{ card.requests > 1 ? 's' : '' }}
              </span>
              <span class="text-2xs text-muted">{{ card.meta }}</span>
            </span>
            <span
              class="inline-flex h-7.5 shrink-0 items-center gap-1 rounded-[9px] border px-2.75 text-xs font-semibold"
              :class="card.setup ? 'border-transparent bg-brand text-on-brand' : 'border-line'"
            >
              {{ card.setup ? 'Set up' : 'Manage' }}<Icon :name="ChevronRight" :size="14" />
            </span>
          </span>
        </RouterLink>
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
        <span class="mt-0.5 block text-2xs text-muted">Plugins are a manifest and a few Luau files. Start from the Discord plugin source.</span>
      </span>
      <span class="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-accent phone:hidden">Read the guide<Icon :name="ArrowUpRight" :size="13" /></span>
    </a>
  </div>
</template>
