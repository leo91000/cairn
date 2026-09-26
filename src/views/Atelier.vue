<script setup lang="ts">
import type { IconName } from '../icons'
import { computed } from 'vue'
import { signOut, state } from '../api'
import Icon from '../components/Icon.vue'
import ThemeControl from '../components/ThemeControl.vue'
import { Activity, ArrowRight, BookOpen, FolderGit2, LogOut, Plug, Robot, Settings } from '../icons'

// The Atelier gathers what equips the agents: resources, connections, the run journal and settings.
const resources = computed<Array<{ to: string, label: string, icon: IconName, count: number, detail: string }>>(() => [
  { to: '/agents', label: 'Agents', icon: Robot, count: state.agents.length, detail: 'Models, access and skills' },
  { to: '/projects', label: 'Projects', icon: FolderGit2, count: state.projects.length, detail: 'Repositories and snapshots' },
  { to: '/skills', label: 'Skills', icon: BookOpen, count: state.skills.length, detail: 'Shared instructions' },
  { to: '/mcps', label: 'MCPs', icon: Plug, count: state.mcps.length, detail: 'MCP servers and the tools agents can call' },
])
const workspace: Array<{ to: string, label: string, icon: IconName, detail: string }> = [
  { to: '/connections', label: 'Connections', icon: Plug, detail: 'Codex, Claude Code, GitHub and 1Password' },
  { to: '/nodes', label: 'Nodes', icon: Activity, detail: 'Trusted machines, capabilities and resource ceilings' },
  { to: '/runs', label: 'Runs', icon: Activity, detail: 'The run journal: every run, with its log and files' },
  { to: '/settings', label: 'Settings', icon: Settings, detail: 'Notifications, access and security' },
]
</script>

<template>
  <div class="atelier mx-auto w-full max-w-240 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <p class="eyebrow">
      Your workspace
    </p>
    <h1 class="mt-1 font-heading text-[28px]! font-extrabold leading-none! tracking-[-0.03em]! phone:text-[34px]!">
      Atelier
    </h1>
    <p class="mt-2 text-[13px] text-muted">
      What your agents work with.
    </p>

    <section class="mt-8" aria-labelledby="atelier-resources">
      <h2 id="atelier-resources" class="eyebrow mb-3 text-base!">
        Resources
      </h2>
      <div class="grid grid-cols-4 gap-3 tablet:grid-cols-2">
        <RouterLink v-for="item in resources" :key="item.to" :to="item.to" :aria-label="item.label" :aria-description="`${item.count} · ${item.detail}`" class="lift flex flex-col gap-3 rounded-2xl border border-line bg-surface p-4 text-ink">
          <span class="flex items-center justify-between">
            <span class="grid size-9 place-items-center rounded-xl bg-soft text-accent"><Icon :name="item.icon" :size="18" /></span>
            <span class="font-heading text-2xl font-extrabold tabular-nums">{{ item.count }}</span>
          </span>
          <span>
            <span class="block text-sm font-semibold">{{ item.label }}</span>
            <span class="mt-0.5 block text-xs text-muted">{{ item.detail }}</span>
          </span>
        </RouterLink>
      </div>
    </section>

    <section class="mt-8" aria-labelledby="atelier-workspace">
      <h2 id="atelier-workspace" class="eyebrow mb-3 text-base!">
        Workspace
      </h2>
      <div class="overflow-hidden rounded-2xl border border-line bg-surface">
        <RouterLink v-for="item in workspace" :key="item.to" :to="item.to" :aria-label="item.label" :aria-description="item.detail" class="group flex items-center gap-3 border-b border-line px-4 py-3.5 text-ink last:border-0 hover:bg-hover">
          <span class="grid size-9 shrink-0 place-items-center rounded-xl bg-hover text-muted group-hover:text-accent"><Icon :name="item.icon" :size="18" /></span>
          <span class="min-w-0 flex-1">
            <span class="block text-sm font-semibold">{{ item.label }}</span>
            <span class="block truncate text-xs text-muted">{{ item.detail }}</span>
          </span>
          <Icon :name="ArrowRight" :size="16" class="text-subtle transition-transform group-hover:translate-x-0.5" />
        </RouterLink>
      </div>
    </section>

    <section class="mt-8" aria-labelledby="atelier-appearance">
      <h2 id="atelier-appearance" class="eyebrow mb-3 text-base!">
        Appearance
      </h2>
      <ThemeControl />
    </section>

    <button type="button" class="press mt-8 hidden h-10 phone:flex items-center gap-2 rounded-full px-4 text-sm font-semibold text-coral hover:bg-coral-soft" :disabled="state.signingOut" @click="signOut">
      <Icon :name="LogOut" :size="16" />Sign out
    </button>
  </div>
</template>
