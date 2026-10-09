<script setup lang="ts">
import type { IconName } from './icons'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  provide,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAgentPortraits } from './agent-avatars'
import {
  ownerPage,
  refresh,
  signOut,
  state,
} from './api'
import { useChatList } from './chat-list'
import CommandPalette from './components/CommandPalette.vue'
import Icon from './components/Icon.vue'
import ShortcutSheet from './components/ShortcutSheet.vue'
import ThemeControl from './components/ThemeControl.vue'
import {
  CalendarClock,
  Check,
  Inbox,
  Keyboard,
  Layers,
  LogOut,
  Plus,
  Search,
  Zap,
} from './icons'
import { useMissionRuns } from './mission-runs'
import { modifier, typingTarget } from './shortcuts'
import { filOf } from './signal'
import { workspaceActionsKey } from './workspace-actions'

useAgentPortraits()
const router = useRouter()
const route = useRoute()
const paletteOpen = ref(false)
const shortcutsOpen = ref(false)
provide(workspaceActionsKey, {
  search: () => { paletteOpen.value = true },
  navigation: () => { void router.push('/') },
})

// The three places of « Signal »: the Fil (home and conversations), Missions and the Atelier.
const atelierPaths = ['/atelier', '/agents', '/projects', '/skills', '/mcps', '/connections', '/nodes', '/settings', '/runs']
const atelierSections = computed(() => [
  { to: '/agents', label: 'Agents' },
  { to: '/projects', label: 'Projects' },
  { to: '/skills', label: 'Skills' },
  { to: '/mcps', label: 'MCPs' },
  { to: '/connections', label: 'Connections' },
  { to: '/runs', label: 'Runs' },
  { to: '/nodes', label: 'Nodes' },
  { to: '/settings', label: 'Settings' },
].filter(item => state.installationRole === 'owner' || !ownerPage(item.to)))
const atelierSection = computed(() => atelierSections.value.find(item => route.path === item.to || (item.to === '/settings' && route.path.startsWith('/settings/'))))
const places: Array<{
  to: string
  label: string
  icon: IconName
  keys: string
}> = [
  {
    to: '/',
    label: 'Fil',
    icon: Inbox,
    keys: 'G F',
  },
  {
    to: '/tasks',
    label: 'Missions',
    icon: CalendarClock,
    keys: 'G M',
  },
  {
    to: '/atelier',
    label: 'Atelier',
    icon: Layers,
    keys: 'G A',
  },
]
watch(() => [state.installationRole, route.path], () => {
  if (state.installationRole === 'member' && ownerPage(route.path))
    void router.replace('/atelier')
}, { immediate: true })

const place = computed(() => route.path === '/' || route.path.startsWith('/chats') ? 0 : route.path.startsWith('/tasks') ? 1 : atelierPaths.some(path => route.path.startsWith(path)) ? 2 : -1)
// Reading screens take the whole phone; the dock returns on the three places.
const reading = computed(() => route.path.startsWith('/chats') || /^\/runs\/./.test(route.path) || route.path === '/authorize' || route.path.startsWith('/settings/'))
const chats = useChatList()
const runs = useMissionRuns({ eager: false })
const fil = computed(() => filOf(chats.value, state.tasks, runs.value))
const needsYou = computed(() => fil.value.forYou.length)
const missionRunning = computed(() => fil.value.live.some(item => item.taskId && item.kind === 'running'))

onMounted(async () => {
  try {
    if (state.authenticated)
      await refresh()
  }
  catch (e) {
    state.error = (e as Error).message
    state.ready = true
  }

  document.addEventListener('keydown', key)
})
onBeforeUnmount(() => document.removeEventListener('keydown', key))
watch(() => route.fullPath, () => {
  paletteOpen.value = false
})

// Global shortcuts: ⌘K anywhere; single keys only outside fields and dialogs. G starts a chord.
let chord = 0
const chordHint = ref(false)

function key(event: KeyboardEvent) {
  if (!state.authenticated)
    return
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
    event.preventDefault()
    paletteOpen.value = !paletteOpen.value
    return
  }

  if (event.key === 'Escape' && event.target instanceof HTMLTextAreaElement && event.target.closest('.chat-composer')) {
    event.target.blur()
    return
  }

  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey || event.isComposing || typingTarget(event.target) || document.querySelector('dialog[open]'))
    return
  const letter = event.key.toLowerCase()
  if (chord && Date.now() - chord < 1200) {
    chord = 0
    chordHint.value = false
    const destination = { f: '/', m: '/tasks', a: '/atelier' }[letter]
    if (destination) {
      event.preventDefault()
      void router.push(destination)
    }

    return
  }

  if (letter === 'g') {
    chord = Date.now()
    chordHint.value = true
    setTimeout(() => chordHint.value = false, 1200)
  }
  else if (event.key === '?') {
    event.preventDefault()
    shortcutsOpen.value = true
  }
  else if (letter === 'c') {
    event.preventDefault()
    void router.push('/chats')
  }
  else if (letter === 'r') {
    const composer = document.querySelector<HTMLTextAreaElement>('.chat-composer textarea')
    if (composer) {
      event.preventDefault()
      composer.focus()
    }
  }
}

function openShortcuts() {
  paletteOpen.value = false
  shortcutsOpen.value = true
}
</script>

<template>
  <div v-if="!state.ready" class="loading-screen min-h-dvh grid place-items-center text-muted">
    Loading…
  </div>
  <div v-else class="shell flex min-h-0 overflow-hidden" :class="state.installationId ? 'flex-1' : 'h-dvh'">
    <nav class="rail z-30 flex w-19 shrink-0 flex-col items-center gap-1 border-r border-line bg-canvas pb-4 pt-4 phone:hidden" aria-label="Workspace navigation">
      <RouterLink to="/" class="press mb-4 grid size-10 place-items-center rounded-xl bg-brand text-white shadow-[0_6px_16px_-6px_#4545ef99]" aria-label="Cairn home">
        <Icon :name="Zap" :size="20" />
      </RouterLink>
      <div class="relative flex flex-col gap-1">
        <span v-if="place >= 0" class="rail-pill absolute left-0 top-0 h-15 w-15 rounded-2xl bg-surface shadow-arcade" :style="{ transform: `translateY(${place * 64}px)` }" />
        <RouterLink
          v-for="(item, index) in places"
          :key="item.to"
          :to="item.to"
          class="rail-item relative grid h-15 w-15 place-items-center rounded-2xl text-muted transition-colors hover:text-ink"
          :class="{ 'text-ink!': place === index }"
          :aria-current="place === index ? 'page' : undefined"
          :aria-label="item.label"
          :aria-description="index === 0 && needsYou ? `${needsYou} need you` : undefined"
          :data-tip="item.label"
          :data-kbd="item.keys"
          data-side="right"
        >
          <span class="relative flex flex-col items-center gap-1">
            <Icon :name="item.icon" :size="20" />
            <span class="text-[10.5px] font-semibold">{{ item.label }}</span>
            <span
              v-if="index === 0 && needsYou"
              class="absolute -right-2.5 -top-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-coral px-1 text-[10px] font-bold text-white"
              aria-hidden="true"
              :title="`${needsYou} need you`"
            >{{ needsYou }}</span>
            <span
              v-if="index === 1 && missionRunning"
              class="live-dot absolute -right-1.5 -top-1"
              aria-hidden="true"
              title="A mission is running"
            />
          </span>
        </RouterLink>
      </div>
      <RouterLink
        to="/chats"
        class="press mt-4 grid size-12 place-items-center rounded-full bg-accent text-surface shadow-[0_10px_24px_-10px_var(--color-accent)]"
        aria-label="New conversation"
        data-tip="New conversation"
        data-kbd="C"
        data-side="right"
      >
        <Icon :name="Plus" :size="22" />
      </RouterLink>
      <div class="mt-auto flex flex-col items-center gap-1">
        <button
          type="button"
          class="grid size-10 place-items-center rounded-xl text-muted hover:bg-hover hover:text-ink"
          aria-label="Search workspace"
          data-tip="Search"
          :data-kbd="`${modifier} K`"
          data-side="right"
          @click="paletteOpen = true"
        >
          <Icon :name="Search" :size="19" />
        </button>
        <button
          type="button"
          class="grid size-10 place-items-center rounded-xl text-muted hover:bg-hover hover:text-ink"
          aria-label="Keyboard shortcuts"
          data-tip="Shortcuts"
          data-kbd="?"
          data-side="right"
          @click="shortcutsOpen = true"
        >
          <Icon :name="Keyboard" :size="19" />
        </button>
        <ThemeControl compact />
        <button
          type="button"
          class="grid size-10 place-items-center rounded-xl text-muted hover:bg-coral-soft hover:text-coral"
          aria-label="Sign out"
          data-tip="Sign out"
          data-side="right"
          :disabled="state.signingOut"
          @click="signOut"
        >
          <Icon :name="LogOut" :size="18" />
        </button>
      </div>
    </nav>
    <main
      class="page flex-1 min-h-0 min-w-0 overflow-auto overscroll-contain [scrollbar-width:thin] [&.page-default]:mx-auto [&.page-default]:w-full [&.page-default]:max-w-340 [&.page-default]:px-9 [&.page-default]:pt-8 [&.page-default]:pb-7 tablet:[&.page-default]:px-6 phone:[&.page-default]:px-3 phone:[&.page-default]:pt-[max(14px,env(safe-area-inset-top))] phone:[&.page-default]:pb-32 [&.page-tasks]:flex [&.page-tasks]:flex-col [&.page-tasks]:overflow-hidden phone:[&.page-tasks]:pb-24 [&.page-run]:flex [&.page-run]:flex-col [&.page-run]:overflow-hidden [&.page-fil]:overflow-hidden"
      :class="{
        'page-fil': route.path === '/',
        'page-tasks': route.path === '/tasks',
        'page-run': /^\/runs\/./.test(route.path) || route.path.startsWith('/chats'),
        'page-chat': route.path.startsWith('/chats'),
        'page-default': route.path !== '/' && !route.path.startsWith('/chats'),
      }"
    >
      <nav v-if="atelierSection" class="atelier-sections -mx-1 mb-6 flex items-center gap-1 overflow-x-auto px-1 pb-1 [scrollbar-width:none] phone:mb-4" aria-label="Atelier sections">
        <RouterLink to="/atelier" class="grid size-9 shrink-0 place-items-center rounded-full text-muted hover:bg-hover hover:text-ink" aria-label="Atelier overview">
          <Icon :name="Layers" :size="17" />
        </RouterLink>
        <RouterLink
          v-for="item in atelierSections"
          :key="item.to"
          :to="item.to"
          class="press shrink-0 whitespace-nowrap rounded-full px-3.5 py-2 text-[13px] font-semibold transition-colors"
          :class="item === atelierSection ? 'bg-ink text-canvas' : 'text-muted hover:bg-hover hover:text-ink'"
          :aria-current="item === atelierSection ? 'page' : undefined"
        >
          {{ item.label }}
        </RouterLink>
      </nav>
      <!-- Chats stays mounted between conversations so switching swaps content in place. -->
      <RouterView v-if="state.installationRole === 'owner' || !ownerPage(route.path)" v-slot="{ Component }">
        <component :is="Component" :key="route.path.startsWith('/chats/') ? '/chats' : route.path">
          <template #account>
            <slot name="settings-account" />
          </template>
          <template #installation>
            <slot name="settings-installation" />
          </template>
          <template v-if="$slots['settings-installation-heading']" #installation-heading>
            <slot name="settings-installation-heading" />
          </template>
          <template #sensitive>
            <slot name="settings-sensitive" />
          </template>
        </component>
      </RouterView>
    </main>
    <nav v-if="!reading" class="dock fixed bottom-[max(14px,env(safe-area-inset-bottom))] left-1/2 z-30 hidden -translate-x-1/2 items-center gap-1 rounded-full border border-line bg-surface/90 p-1.5 shadow-lift backdrop-blur-md phone:flex" aria-label="Quick navigation">
      <span v-if="place >= 0" class="dock-pill absolute left-1.5 top-1.5 h-13 w-19 rounded-full bg-hover" :style="{ transform: `translateX(${place * 80}px)` }" />
      <RouterLink
        v-for="(item, index) in places"
        :key="item.to"
        :to="item.to"
        class="relative grid h-13 w-19 place-items-center rounded-full text-muted"
        :class="{ 'text-ink!': place === index }"
        :aria-label="item.label"
        :aria-current="place === index ? 'page' : undefined"
      >
        <span class="relative flex flex-col items-center gap-0.5">
          <Icon :name="item.icon" :size="20" />
          <span class="text-[10.5px] font-semibold">{{ item.label }}</span>
          <span v-if="index === 0 && needsYou" class="absolute -right-3 -top-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-coral px-1 text-[10px] font-bold text-white" aria-hidden="true">{{ needsYou }}</span>
          <span v-if="index === 1 && missionRunning" class="live-dot absolute -right-2 -top-1" aria-hidden="true" />
        </span>
      </RouterLink>
      <RouterLink to="/chats" class="press ml-1 grid size-13 place-items-center rounded-full bg-accent text-surface shadow-[0_8px_20px_-8px_var(--color-accent)]" aria-label="New conversation">
        <Icon :name="Plus" :size="22" />
      </RouterLink>
    </nav>
    <Transition name="signal-pop">
      <p v-if="chordHint" class="fixed bottom-6 left-1/2 z-40 m-0 flex -translate-x-1/2 items-center gap-2 rounded-full bg-bubble px-4 py-2 text-xs font-semibold text-on-bubble shadow-lift" role="status">
        <kbd class="keycap">G</kbd> then <kbd class="keycap">F</kbd> Fil · <kbd class="keycap">M</kbd> Missions · <kbd class="keycap">A</kbd> Atelier
      </p>
    </Transition>
  </div>
  <Transition name="toast">
    <div v-if="state.toast" class="toast fixed bottom-[25px] left-[50%] [transform:translateX(-50%)] bg-bubble text-on-bubble rounded-full [box-shadow:var(--shadow-lift)] z-100 flex items-center gap-[9px] text-xs font-medium max-w-[calc(100vw_-_30px)] phone:bottom-[calc(96px_+_env(safe-area-inset-bottom))] px-5 py-[12px]" role="status">
      <Icon :name="Check" :size="17" />{{ state.toast }}
    </div>
  </Transition>
  <CommandPalette v-if="paletteOpen" @close="paletteOpen = false" @shortcuts="openShortcuts" />
  <ShortcutSheet v-if="shortcutsOpen" @close="shortcutsOpen = false" />
</template>

<style scoped>
.rail-pill, .dock-pill { transition: transform 0.36s var(--ease-spring); }
/* Each place rises in as it opens. */
.page > :deep(*) { animation: page-in 0.32s var(--ease-signal) both; }
@keyframes page-in { from { opacity: 0; transform: translateY(6px); } }
@media (prefers-reduced-motion: reduce) {
  .rail-pill, .dock-pill { transition: none; }
  .page > :deep(*) { animation: none; }
}
</style>
