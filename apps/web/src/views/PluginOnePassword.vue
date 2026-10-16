<script setup lang="ts">
import type { IconName } from '../icons'
import type { Catalog } from '../plugins'
import { computed, onMounted, ref } from 'vue'
import { api } from '../api'
import Icon from '../components/Icon.vue'
import OnePasswordAccounts from '../components/OnePasswordAccounts.vue'
import PluginMark from '../components/PluginMark.vue'
import {
  ChevronRight,
  KeyRound,
  ShieldCheck,
  Terminal,
} from '../icons'

// The built-in 1Password plugin: its service accounts and which agents may read through them.
const catalog = ref<Catalog>()
const manifest = computed(() => catalog.value?.packages.find(item => item.id === 'onepassword')?.manifest)
const capabilities: [IconName, string][] = [[Terminal, 'CLI tool'], [KeyRound, 'Secrets'], [ShieldCheck, 'Per-agent grants']]

onMounted(async () => {
  catalog.value = await api<Catalog>('/plugins').catch(() => undefined)
})
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <nav class="flex items-center gap-1.5 text-xs font-medium text-muted" aria-label="Breadcrumb">
      <RouterLink to="/atelier" class="hover:text-ink phone:hidden">
        Atelier
      </RouterLink><Icon :name="ChevronRight" :size="12" class="phone:hidden" />
      <RouterLink to="/plugins" class="hover:text-ink">
        Plugins
      </RouterLink><Icon :name="ChevronRight" :size="12" /><span class="text-ink">1Password</span>
    </nav>
    <div class="mt-4.5 flex items-center gap-4">
      <PluginMark plugin="onepassword" :size="56" />
      <div>
        <h1 class="font-heading text-[26px]! font-extrabold leading-none! tracking-[-0.03em]!">
          1Password
        </h1>
        <p class="mt-2 flex flex-wrap items-center gap-2 text-2xs text-muted">
          <span class="rounded-full bg-soft px-2 py-0.5 font-semibold text-accent">Built-in</span>
          <span v-if="manifest">v{{ manifest.version }} · Luau sandbox</span>
        </p>
      </div>
    </div>
    <p class="mt-4 max-w-150 text-sm leading-relaxed text-muted">
      {{ manifest?.description || 'Agents pipe secrets straight into commands. Values never appear in the conversation.' }}
    </p>
    <div class="mt-3 flex flex-wrap gap-1.5">
      <span v-for="[icon, label] in capabilities" :key="label" class="inline-flex h-6 items-center gap-1.25 rounded-[7px] border border-line bg-inset px-2.25 text-2xs font-medium text-muted">
        <Icon :name="icon" :size="12" />{{ label }}
      </span>
    </div>

    <div class="mt-6 grid grid-cols-[minmax(0,1fr)_300px] items-start gap-5 tablet:grid-cols-1">
      <OnePasswordAccounts />
      <aside class="rounded-2xl border border-line bg-surface p-4.5" aria-labelledby="onepassword-usage">
        <h2 id="onepassword-usage" class="flex items-center gap-2 text-[13px]! font-semibold">
          <Icon :name="Terminal" :size="16" class="text-accent" />How agents use it
        </h2>
        <p class="mt-2 text-xs leading-relaxed text-muted">
          An authorized agent reads a secret with the Cairn CLI and pipes it into the command that needs it:
        </p>
        <pre class="mt-3 overflow-x-auto rounded-xl border border-line bg-inset px-3 py-2.5 font-mono text-3xs leading-relaxed"><code>cairn plugin onepassword accounts
cairn plugin onepassword read \
  --account ACCOUNT_ID \
  op://vault/item/field \
  | docker login --password-stdin …</code></pre>
        <ul class="mt-3 grid gap-2 text-2xs leading-relaxed text-muted">
          <li class="flex items-start gap-2">
            <Icon :name="ShieldCheck" :size="13" class="mt-px shrink-0 text-success" />Values go to the command, never to the conversation.
          </li>
          <li class="flex items-start gap-2">
            <Icon :name="KeyRound" :size="13" class="mt-px shrink-0 text-success" />Tokens stay encrypted on the installation. Agents only get read access.
          </li>
        </ul>
      </aside>
    </div>
  </div>
</template>
