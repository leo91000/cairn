<script setup lang="ts">
import type { IconName } from '../icons'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { api, notify, state } from '../api'
import { chatList, refreshChats } from '../chat-list'
import {
  Activity,
  BookOpen,
  CalendarClock,
  CornerDownLeft,
  FolderGit2,
  Inbox,
  Keyboard,
  Layers,
  Moon,
  Play,
  Plug,
  Plus,
  Search,
  Settings,
  X,
} from '../icons'
import { refreshMissionRuns, useMissionRuns } from '../mission-runs'
import { modifier } from '../shortcuts'
import { filOf } from '../signal'
import { setThemePreference, themePreference } from '../theme'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'

// Universal search with direct actions: conversations, missions, agents, projects and skills.
const emit = defineEmits<{ close: [], shortcuts: [] }>()
const router = useRouter()
const runs = useMissionRuns()
const dialog = ref<HTMLDialogElement>()
const input = ref<HTMLInputElement>()
const list = ref<HTMLElement>()
const query = ref('')
const active = ref(0)

interface Command {
  id: string
  group: string
  label: string
  hint?: string
  icon?: IconName
  avatar?: { name: string, key: string }
  keys?: string[]
  keywords?: string
  run: () => unknown
}

const go = (to: string) => () => router.push(to)

async function runMission(id: string, name: string) {
  try {
    await api(`/tasks/${id}/run`, { method: 'POST' })
    notify(`${name} started`)
    await refreshMissionRuns()
  }
  catch (error) {
    notify((error as Error).message)
  }
}

const commands = computed<Command[]>(() => {
  const items: Command[] = [
    {
      id: 'new',
      group: 'Actions',
      label: 'New conversation',
      icon: Plus,
      keys: ['C'],
      run: go('/chats'),
    },
    {
      id: 'fil',
      group: 'Actions',
      label: 'Go to the Fil',
      icon: Inbox,
      keys: ['G', 'F'],
      run: go('/'),
    },
    {
      id: 'missions',
      group: 'Actions',
      label: 'Go to Missions',
      icon: CalendarClock,
      keys: ['G', 'M'],
      run: go('/tasks'),
    },
    {
      id: 'atelier',
      group: 'Actions',
      label: 'Go to the Atelier',
      icon: Layers,
      keys: ['G', 'A'],
      run: go('/atelier'),
    },
    {
      id: 'journal',
      group: 'Actions',
      label: 'Open the run journal',
      icon: Activity,
      keywords: 'runs history activity',
      run: go('/runs'),
    },
    {
      id: 'new-mission',
      group: 'Actions',
      label: 'New mission',
      icon: CalendarClock,
      keywords: 'task schedule',
      run: go('/tasks?new=1'),
    },
    {
      id: 'theme',
      group: 'Actions',
      label: themePreference.value === 'dark' ? 'Switch to the light theme' : 'Switch to the dark theme',
      icon: Moon,
      keywords: 'appearance dark light theme',
      run: () => setThemePreference(themePreference.value === 'dark' ? 'light' : 'dark'),
    },
    {
      id: 'connections',
      group: 'Actions',
      label: 'Connections',
      icon: Plug,
      keywords: 'codex claude github 1password accounts',
      run: go('/connections'),
    },
    {
      id: 'settings',
      group: 'Actions',
      label: 'Settings',
      icon: Settings,
      keywords: 'notifications appearance password',
      run: go('/settings'),
    },
    {
      id: 'keys',
      group: 'Actions',
      label: 'Keyboard shortcuts',
      icon: Keyboard,
      keys: ['?'],
      run: () => emit('shortcuts'),
    },
  ]
  if (state.installationRole === 'member') {
    for (let index = items.length - 1; index >= 0; index--) {
      if (['connections', 'settings'].includes(items[index]!.id))
        items.splice(index, 1)
    }
  }

  const fil = filOf(chatList.value, state.tasks, runs.value)
  for (const item of fil.forYou) {
    items.push({
      id: `need-${item.key}`,
      group: 'Needs you',
      label: item.title,
      hint: item.subtitle,
      avatar: { name: item.agent, key: item.agentKey },
      run: go(item.to),
    })
  }

  const needs = new Set(fil.forYou.map(item => item.key))
  for (const chat of [...chatList.value].sort((a, b) => b.updatedAt - a.updatedAt)) {
    if (!needs.has(`chat:${chat.id}`)) {
      items.push({
        id: `chat-${chat.id}`,
        group: 'Conversations',
        label: chat.title || 'New conversation',
        hint: [chat.agentName, chat.projectName].filter(Boolean).join(' · '),
        avatar: { name: chat.agentName, key: chat.agentId },
        run: go(`/chats/${chat.id}`),
      })
    }
  }

  for (const task of state.tasks.filter(task => !task.archived)) {
    items.push({
      id: `task-${task.id}`,
      group: 'Missions',
      label: task.name,
      hint: task.enabled ? 'Mission' : 'Paused mission',
      icon: CalendarClock,
      keywords: task.prompt.slice(0, 400),
      run: go(`/tasks?task=${task.id}`),
    })
    items.push({
      id: `run-${task.id}`,
      group: 'Missions',
      label: `Run “${task.name}” now`,
      icon: Play,
      run: () => runMission(task.id, task.name),
    })
  }

  for (const agent of state.agents) {
    items.push({
      id: `agent-${agent.id}`,
      group: 'Agents',
      label: `New conversation with ${agent.name}`,
      hint: agent.description,
      avatar: { name: agent.name, key: agent.id },
      run: go(`/chats?agent=${agent.id}`),
    })
  }

  for (const project of state.projects) {
    items.push({
      id: `project-${project.id}`,
      group: 'Projects',
      label: project.name,
      hint: 'Project',
      icon: FolderGit2,
      run: go('/projects'),
    })
  }

  for (const skill of state.skills) {
    items.push({
      id: `skill-${skill.name}`,
      group: 'Skills',
      label: skill.name,
      hint: skill.description,
      icon: BookOpen,
      run: go('/skills'),
    })
  }

  return items
})

function score(command: Command, text: string) {
  const label = command.label.toLowerCase()
  if (label.startsWith(text))
    return 100
  if (label.split(/\s+/).some(word => word.replace(/\W/g, '').startsWith(text)))
    return 80
  if (label.includes(text))
    return 60
  if ((command.hint ?? '').toLowerCase().includes(text))
    return 40
  return (command.keywords ?? '').toLowerCase().includes(text) ? 20 : 0
}

const results = computed(() => {
  const text = query.value.trim().toLowerCase()
  if (!text) {
    return [
      ...commands.value.filter(item => item.group === 'Actions').slice(0, 4),
      ...commands.value.filter(item => item.group === 'Needs you'),
      ...commands.value.filter(item => item.group === 'Conversations').slice(0, 5),
    ]
  }

  const ranked = commands.value.map(item => ({ item, value: score(item, text) })).filter(entry => entry.value > 0).sort((a, b) => b.value - a.value).slice(0, 14).map(entry => entry.item)
  const ask: Command = {
    id: 'ask',
    group: 'Ask',
    label: `Start a conversation: “${query.value.trim()}”`,
    icon: Plus,
    hint: 'With the main agent',
    run: () => router.push({ path: '/chats', query: { draft: query.value.trim() } }),
  }
  return [...ranked, ask]
})
const grouped = computed(() => {
  const groups: Array<{ name: string, items: Array<{ command: Command, index: number }> }> = []
  results.value.forEach((command, index) => {
    const group = groups.find(item => item.name === command.group) ?? groups[groups.push({ name: command.group, items: [] }) - 1]
    group.items.push({ command, index })
  })
  return groups
})
watch(query, () => active.value = 0)
watch(active, async () => {
  await nextTick()
  list.value?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' })
})
// Closing returns focus to the control that opened the palette.
const previous = document.activeElement as HTMLElement | null
onBeforeUnmount(() => {
  // A modal dialog keeps the page inert until it closes.
  dialog.value?.close()
  previous?.focus()
})
onMounted(() => {
  dialog.value?.showModal()
  input.value?.focus()
  void refreshChats()
})

function run(command: Command) {
  emit('close')
  void command.run()
}

function keydown(event: KeyboardEvent) {
  const total = results.value.length
  if (event.key === 'ArrowDown' || (event.ctrlKey && event.key === 'n')) {
    event.preventDefault()
    active.value = (active.value + 1) % total
  }
  else if (event.key === 'ArrowUp' || (event.ctrlKey && event.key === 'p')) {
    event.preventDefault()
    active.value = (active.value - 1 + total) % total
  }
  else if (event.key === 'Enter' && !event.isComposing) {
    event.preventDefault()
    const command = results.value[active.value]
    if (command)
      run(command)
  }
}

function cancel() {
  if (query.value)
    query.value = ''
  else
    emit('close')
}
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="command-palette m-0 mx-auto mt-[12vh] flex max-h-[min(560px,80dvh)] w-[calc(100%_-_24px)] max-w-[640px] flex-col overflow-hidden rounded-[22px] border border-line bg-surface p-0 text-ink shadow-pop open:flex phone:mt-[max(12px,env(safe-area-inset-top))]"
      aria-label="Search and commands"
      @cancel.prevent="cancel"
      @click="event => { if (event.target === dialog) emit('close') }"
      @keydown="keydown"
    >
      <label class="flex h-14 shrink-0 flex-row! items-center gap-3 border-b border-line px-4">
        <Icon :name="Search" :size="20" class="text-muted" />
        <input
          ref="input"
          v-model="query"
          class="min-h-0! min-w-0 flex-1 border-0! bg-transparent! p-0! text-[16px] text-ink shadow-none! outline-none placeholder:text-subtle"
          placeholder="Search conversations, missions, or type a command…"
          aria-label="Search"
          role="combobox"
          aria-expanded="true"
          aria-controls="command-results"
          :aria-activedescendant="`command-${active}`"
        >
        <kbd class="keycap phone:hidden">Esc</kbd>
        <button
          type="button"
          class="hidden size-8 place-items-center rounded-full hover:bg-hover phone:grid"
          aria-label="Close search"
          @click="emit('close')"
        >
          <Icon :name="X" :size="16" />
        </button>
      </label>
      <div
        id="command-results"
        ref="list"
        class="min-h-0 flex-1 overflow-y-auto p-2 [scrollbar-width:thin]"
        role="listbox"
      >
        <template v-for="group in grouped" :key="group.name">
          <p class="eyebrow px-3 pb-1.5 pt-2.5">
            {{ group.name }}
          </p>
          <button
            v-for="{ command, index } in group.items"
            :id="`command-${index}`"
            :key="command.id"
            type="button"
            role="option"
            :aria-selected="active === index"
            :data-active="active === index"
            class="flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left transition-colors duration-100"
            :class="active === index ? 'bg-soft' : ''"
            @mousemove="active = index"
            @click="run(command)"
          >
            <AgentAvatar
              v-if="command.avatar"
              :name="command.avatar.name"
              :identity="command.avatar.key"
              :size="24"
            />
            <span v-else class="grid size-6 shrink-0 place-items-center rounded-[7px] bg-hover" :class="active === index ? 'text-accent' : 'text-muted'"><Icon v-if="command.icon" :name="command.icon" :size="14" /></span>
            <span class="min-w-0 flex-1 truncate text-sm" :class="active === index ? 'font-semibold text-accent' : 'text-ink'">{{ command.label }}</span>
            <span v-if="command.hint" class="max-w-[40%] shrink truncate text-xs text-muted phone:hidden">{{ command.hint }}</span>
            <span v-if="command.keys" class="flex shrink-0 gap-1"><kbd v-for="key in command.keys" :key="key" class="keycap">{{ key }}</kbd></span>
            <Icon
              v-else-if="active === index"
              :name="CornerDownLeft"
              :size="14"
              class="text-accent"
            />
          </button>
        </template>
      </div>
      <footer class="flex shrink-0 items-center gap-4 border-t border-line bg-inset px-4 py-2.5 text-[11.5px] text-muted phone:hidden">
        <span class="flex items-center gap-1"><kbd class="keycap">↑</kbd><kbd class="keycap">↓</kbd> move</span>
        <span class="flex items-center gap-1"><kbd class="keycap">↵</kbd> run</span>
        <span class="flex items-center gap-1"><kbd class="keycap">Esc</kbd> close</span>
        <span class="ml-auto flex items-center gap-1"><kbd class="keycap">{{ modifier }}</kbd><kbd class="keycap">K</kbd> anywhere</span>
      </footer>
    </dialog>
  </Teleport>
</template>

<style scoped>
.command-palette::backdrop { background: light-dark(#16151d4d, #000000a6); backdrop-filter: blur(3px); }
.command-palette[open] { animation: palette-in 0.24s var(--ease-spring); }
@keyframes palette-in { from { opacity: 0; transform: translateY(-8px) scale(0.97); } }
@media (prefers-reduced-motion: reduce) { .command-palette[open] { animation: none; } }
</style>
