<script setup lang="ts">
import type { SkillFile } from '../../../../packages/contracts/contracts'
import type { IconName } from '../icons'
import type {
  DraftFile,
  PickedFile,
  SkillFileKind,
  SkillTreeNode,
} from '../skill-bundle'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import {
  BookOpen,
  ChevronRight,
  Download,
  FileBox,
  FileImage,
  FilePlus,
  FileTerminal,
  FileText,
  Folder,
  FolderOpen,
  MoreHorizontal,
  Pencil,
  Terminal,
  Trash2,
  Undo2,
  Upload,
} from '../icons'
import {
  buildSkillTree,
  dirname,
  draftState,
  droppedFiles,
  formatBytes,
  skillFileKind,
  skillManifest,
  visibleTreeRows,
} from '../skill-bundle'
import Icon from './Icon.vue'

// The skill's files as a collapsible tree. Rows select files; their menu (also on
// right click, F2 and Delete) renames, moves, deletes and toggles the executable bit.
// Files and folders dropped from the desktop land in the folder under the pointer.
const props = defineProps<{
  files: DraftFile[]
  deleted: SkillFile[]
  selected: string | null
  readonly: boolean
}>()
const emit = defineEmits<{
  select: [path: string]
  rename: [path: string, folder: boolean]
  remove: [path: string, folder: boolean]
  executable: [path: string]
  download: [path: string]
  create: [folder: string]
  upload: [folder: string]
  restore: [path: string]
  drop: [files: PickedFile[], folder: string]
  move: [path: string, folder: string]
}>()

const collapsed = ref(new Set<string>())
const tree = computed(() => buildSkillTree(props.files))
const rows = computed(() => visibleTreeRows(tree.value, collapsed.value))
const list = ref<HTMLElement>()
const menu = ref<HTMLElement>()
const menuFor = ref<SkillTreeNode<DraftFile>>()
const menuPosition = ref({ left: '0px', top: '0px' })
const dropFolder = ref<string | null>(null)
const draggingPath = ref<string>()
const internalType = 'application/x-cairn-skill-path'

const kindIcons: Record<SkillFileKind, IconName> = {
  manifest: BookOpen,
  markdown: FileText,
  script: FileTerminal,
  image: FileImage,
  text: FileText,
  binary: FileBox,
}
const kindTones: Record<SkillFileKind, string> = {
  manifest: 'text-accent',
  markdown: 'text-muted',
  script: 'text-success',
  image: 'text-claude',
  text: 'text-muted',
  binary: 'text-subtle',
}
const stateLabels = {
  new: { letter: 'A', label: 'New file', tone: 'text-success' },
  modified: { letter: 'M', label: 'Modified', tone: 'text-warning' },
  moved: { letter: 'R', label: 'Renamed', tone: 'text-accent' },
  mode: { letter: 'X', label: 'Executable changed', tone: 'text-accent' },
}

function folderDirty(path: string) {
  return props.files.some(file => file.path.startsWith(`${path}/`) && draftState(file) !== null)
    || props.deleted.some(file => file.path.startsWith(`${path}/`))
}

function toggle(path: string, open?: boolean) {
  const next = new Set(collapsed.value)
  if (open ?? next.has(path))
    next.delete(path)
  else
    next.add(path)
  collapsed.value = next
}

function reveal(path: string) {
  const next = new Set(collapsed.value)
  let folder = dirname(path)
  while (folder) {
    next.delete(folder)
    folder = dirname(folder)
  }

  collapsed.value = next
}

function activate(node: SkillTreeNode<DraftFile>) {
  if (node.type === 'folder')
    toggle(node.path)
  else
    emit('select', node.path)
}

function rowButtons() {
  return Array.from(list.value?.querySelectorAll<HTMLButtonElement>('[data-tree-row]') ?? [])
}

function focusRow(path: string) {
  void nextTick(() => rowButtons().find(button => button.dataset.path === path)?.focus())
}

function keydown(event: KeyboardEvent, node: SkillTreeNode<DraftFile>) {
  const buttons = rowButtons()
  const index = buttons.indexOf(event.currentTarget as HTMLButtonElement)
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault()
    buttons[Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)))]?.focus()
  }
  else if (event.key === 'Home' || event.key === 'End') {
    event.preventDefault()
    buttons[event.key === 'Home' ? 0 : buttons.length - 1]?.focus()
  }
  else if (event.key === 'ArrowRight' && node.type === 'folder') {
    event.preventDefault()
    if (collapsed.value.has(node.path))
      toggle(node.path, true)
    else
      buttons[index + 1]?.focus()
  }
  else if (event.key === 'ArrowLeft') {
    event.preventDefault()
    if (node.type === 'folder' && !collapsed.value.has(node.path))
      toggle(node.path, false)
    else if (dirname(node.path))
      focusRow(dirname(node.path))
  }
  else if (event.key === 'F2' && !props.readonly && node.path !== skillManifest) {
    event.preventDefault()
    emit('rename', node.path, node.type === 'folder')
  }
  else if ((event.key === 'Delete' || event.key === 'Backspace') && !props.readonly && node.path !== skillManifest) {
    event.preventDefault()
    emit('remove', node.path, node.type === 'folder')
  }
  else if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
    event.preventDefault()
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
    openMenu(node, rect.left + 24, rect.bottom + 4)
  }
}

async function openMenu(node: SkillTreeNode<DraftFile>, x: number, y: number) {
  menuFor.value = node
  await nextTick()
  menu.value?.showPopover()
  const width = menu.value?.offsetWidth ?? 220
  const height = menu.value?.offsetHeight ?? 200
  menuPosition.value = {
    left: `${Math.max(8, Math.min(x, innerWidth - width - 8))}px`,
    top: `${Math.max(8, y + height > innerHeight - 8 ? y - height - 8 : y)}px`,
  }
  menu.value?.querySelector<HTMLButtonElement>('[role="menuitem"]')?.focus({ preventScroll: true })
}

function openMenuFrom(event: MouseEvent, node: SkillTreeNode<DraftFile>) {
  const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
  void openMenu(node, rect.right - 220, rect.bottom + 4)
}

function closeMenu(restore = true) {
  const node = menuFor.value
  if (menu.value?.matches(':popover-open'))
    menu.value.hidePopover()
  menuFor.value = undefined
  if (restore && node)
    focusRow(node.path)
}

function act(action: () => void) {
  closeMenu(false)
  action()
}

function menuKeydown(event: KeyboardEvent) {
  event.stopPropagation()
  if (event.key === 'Escape') {
    event.preventDefault()
    closeMenu()
  }
  else if (event.key === 'Tab') {
    closeMenu(false)
  }
  else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    event.preventDefault()
    const items = Array.from(menu.value?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])
    const current = items.indexOf(document.activeElement as HTMLButtonElement)
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (current + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length
    items[next]?.focus()
  }
}

function outside(event: Event) {
  if (menuFor.value && event.target instanceof Node && !menu.value?.contains(event.target))
    closeMenu(false)
}

function targetFolder(event: DragEvent) {
  const row = (event.target as HTMLElement).closest<HTMLElement>('[data-node-path]')
  if (!row)
    return ''
  return row.dataset.nodeType === 'folder' ? row.dataset.nodePath! : dirname(row.dataset.nodePath!)
}

function dragStart(event: DragEvent, node: SkillTreeNode<DraftFile>) {
  if (props.readonly || node.type !== 'file' || node.path === skillManifest) {
    event.preventDefault()
    return
  }

  draggingPath.value = node.path
  event.dataTransfer?.setData(internalType, node.path)
  event.dataTransfer!.effectAllowed = 'move'
}

function dragOver(event: DragEvent) {
  const types = event.dataTransfer?.types ?? []
  if (props.readonly || (!types.includes('Files') && !types.includes(internalType)))
    return
  event.preventDefault()
  event.dataTransfer!.dropEffect = types.includes(internalType) ? 'move' : 'copy'
  dropFolder.value = targetFolder(event)
}

function dragLeave(event: DragEvent) {
  if (!(event.currentTarget as HTMLElement).contains(event.relatedTarget as Node | null))
    dropFolder.value = null
}

async function drop(event: DragEvent) {
  const folder = dropFolder.value ?? targetFolder(event)
  dropFolder.value = null
  if (props.readonly || !event.dataTransfer)
    return
  event.preventDefault()
  const moved = event.dataTransfer.getData(internalType)
  draggingPath.value = undefined
  if (moved) {
    if (dirname(moved) !== folder)
      emit('move', moved, folder)
    return
  }

  const files = await droppedFiles(event.dataTransfer)
  if (files.length) {
    if (folder)
      toggle(folder, true)
    emit('drop', files, folder)
  }
}

onMounted(() => {
  document.addEventListener('pointerdown', outside, true)
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside, true)
  if (menu.value?.matches(':popover-open'))
    menu.value.hidePopover()
})

defineExpose({ reveal })
</script>

<template>
  <div
    class="skill-tree relative flex min-h-0 flex-1 flex-col"
    @dragover="dragOver"
    @dragleave="dragLeave"
    @drop="drop"
  >
    <ul
      ref="list"
      class="m-0! flex-1 list-none overflow-auto overscroll-contain px-2 py-1.5"
      aria-label="Skill files"
    >
      <li
        v-for="node in rows"
        :key="`${node.type}:${node.path}`"
        class="tree-row group relative flex min-h-8.5 items-center rounded-lg transition-[background-color] phone:min-h-11"
        :class="[
          node.type === 'file' && node.path === selected ? 'selected bg-soft text-ink' : 'text-ink hover:bg-hover/70',
          node.type === 'folder' && dropFolder === node.path ? 'bg-soft outline-2 outline-dashed outline-accent/60 -outline-offset-2' : '',
          draggingPath === node.path ? 'opacity-50' : '',
        ]"
        :data-node-path="node.path"
        :data-node-type="node.type"
        :draggable="!readonly && node.type === 'file' && node.path !== skillManifest"
        @dragstart="dragStart($event, node)"
        @dragend="draggingPath = undefined"
        @contextmenu.prevent="openMenu(node, $event.clientX, $event.clientY)"
      >
        <button
          type="button"
          data-tree-row
          :data-path="node.path"
          class="flex min-w-0 flex-1 items-center gap-1.5 self-stretch rounded-lg py-1 pr-1.5 text-left text-sm focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
          :style="{ paddingLeft: `${8 + node.depth * 16}px` }"
          :aria-label="node.path"
          :aria-expanded="node.type === 'folder' ? !collapsed.has(node.path) : undefined"
          :aria-current="node.type === 'file' && node.path === selected ? 'true' : undefined"
          @click="activate(node)"
          @keydown="keydown($event, node)"
        >
          <span class="grid w-3.5 shrink-0 place-items-center text-subtle">
            <Icon
              v-if="node.type === 'folder'"
              :name="ChevronRight"
              :size="13"
              class="transition-transform duration-150"
              :class="collapsed.has(node.path) ? '' : 'rotate-90'"
            />
          </span>
          <template v-if="node.type === 'folder'">
            <Icon :name="collapsed.has(node.path) ? Folder : FolderOpen" :size="16" class="text-accent/80" />
            <span class="min-w-0 flex-1 truncate font-medium">{{ node.name }}</span>
          </template>
          <template v-else>
            <Icon :name="kindIcons[skillFileKind(node.file)]" :size="16" :class="kindTones[skillFileKind(node.file)]" />
            <span class="min-w-0 flex-1 truncate" :class="node.path === skillManifest ? 'font-semibold' : ''">{{ node.name }}</span>
          </template>
        </button>
        <span class="pointer-events-none flex shrink-0 items-center gap-1.5 pr-1 text-3xs tabular-nums text-subtle">
          <template v-if="node.type === 'file'">
            <span
              v-if="node.file.executable"
              class="rounded-[4px] bg-success/12 px-1 py-px font-mono text-3xs font-semibold text-success"
              title="Executable"
              aria-label="Executable"
            >+x</span>
            <span class="group-hover:hidden group-focus-within:hidden phone:hidden">{{ formatBytes(node.file.size) }}</span>
            <span
              v-if="draftState(node.file)"
              class="w-3 text-center font-mono text-2xs font-bold"
              :class="stateLabels[draftState(node.file)!].tone"
              :title="stateLabels[draftState(node.file)!].label"
              :aria-label="stateLabels[draftState(node.file)!].label"
            >{{ stateLabels[draftState(node.file)!].letter }}</span>
          </template>
          <template v-else>
            <span class="group-hover:hidden group-focus-within:hidden phone:hidden">{{ node.files }}</span>
            <span
              v-if="folderDirty(node.path)"
              class="mx-1 size-1.5 rounded-full bg-warning"
              title="Contains changes"
              aria-label="Contains changes"
            />
          </template>
        </span>
        <button
          type="button"
          class="mr-0.5 hidden size-7 shrink-0 place-items-center rounded-md text-muted hover:bg-hover hover:text-ink focus-visible:grid focus-visible:outline-2 focus-visible:outline-accent group-hover:grid group-focus-within:grid phone:grid phone:size-9"
          :aria-label="`Actions for ${node.path}`"
          tabindex="-1"
          @click.stop="openMenuFrom($event, node)"
        >
          <Icon :name="MoreHorizontal" :size="16" />
        </button>
      </li>
    </ul>

    <div v-if="deleted.length" class="border-t border-line px-2 py-2">
      <p class="m-0! px-2 pb-1 text-3xs font-semibold tracking-[0.08em] text-subtle uppercase">
        Deleted · {{ deleted.length }}
      </p>
      <ul class="m-0! list-none">
        <li v-for="file in deleted" :key="file.path" class="flex min-h-8 items-center gap-2 rounded-lg px-2 text-sm text-subtle">
          <Icon :name="Trash2" :size="14" class="text-danger/70" />
          <span class="min-w-0 flex-1 truncate line-through" :title="file.path">{{ file.path }}</span>
          <button
            v-if="!readonly"
            type="button"
            class="grid size-7 shrink-0 place-items-center rounded-md text-muted hover:bg-hover hover:text-ink phone:size-9"
            :aria-label="`Restore ${file.path}`"
            :disabled="files.some(entry => entry.path === file.path)"
            :title="files.some(entry => entry.path === file.path) ? 'Another file uses this path' : 'Restore'"
            @click="emit('restore', file.path)"
          >
            <Icon :name="Undo2" :size="14" />
          </button>
        </li>
      </ul>
    </div>

    <div
      v-if="dropFolder !== null"
      class="pointer-events-none absolute inset-x-2 bottom-2 flex items-center justify-center gap-2 rounded-xl border border-dashed border-accent/60 bg-raised/95 px-3 py-2.5 text-xs font-medium text-accent shadow-lift"
      role="status"
    >
      <Icon :name="Upload" :size="15" />
      {{ dropFolder ? `Drop into ${dropFolder}/` : 'Drop into the skill folder' }}
    </div>

    <div
      ref="menu"
      popover="manual"
      role="menu"
      :aria-label="menuFor ? `Actions for ${menuFor.path}` : 'File actions'"
      :style="menuPosition"
      class="fixed inset-auto m-0 w-55 rounded-xl border border-line bg-raised p-1.5 text-ink shadow-lift"
      @keydown="menuKeydown"
    >
      <template v-if="menuFor">
        <template v-if="menuFor.type === 'folder'">
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item"
            @click="act(() => emit('create', menuFor!.path))"
          >
            <Icon :name="FilePlus" :size="15" />New file here
          </button>
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item"
            @click="act(() => emit('upload', menuFor!.path))"
          >
            <Icon :name="Upload" :size="15" />Upload files here
          </button>
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item"
            @click="act(() => emit('rename', menuFor!.path, true))"
          >
            <Icon :name="Pencil" :size="15" />Rename folder<kbd>F2</kbd>
          </button>
          <div v-if="!readonly" role="separator" class="my-1.5 border-t border-line" />
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item text-danger"
            @click="act(() => emit('remove', menuFor!.path, true))"
          >
            <Icon :name="Trash2" :size="15" />Delete folder
          </button>
          <p v-if="readonly" class="m-0! px-2.5 py-2 text-xs text-muted">
            {{ menuFor.files }} files · {{ formatBytes(menuFor.size) }}
          </p>
        </template>
        <template v-else>
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item"
            :disabled="menuFor.path === skillManifest"
            @click="act(() => emit('rename', menuFor!.path, false))"
          >
            <Icon :name="Pencil" :size="15" />Rename or move<kbd>F2</kbd>
          </button>
          <button
            v-if="!readonly"
            type="button"
            role="menuitemcheckbox"
            :aria-checked="menuFor.file.executable"
            tabindex="-1"
            class="tree-menu-item"
            :disabled="menuFor.path === skillManifest"
            @click="act(() => emit('executable', menuFor!.path))"
          >
            <Icon :name="Terminal" :size="15" />{{ menuFor.file.executable ? 'Remove executable' : 'Make executable' }}
          </button>
          <button
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item"
            @click="act(() => emit('download', menuFor!.path))"
          >
            <Icon :name="Download" :size="15" />Download file
          </button>
          <div v-if="!readonly" role="separator" class="my-1.5 border-t border-line" />
          <button
            v-if="!readonly"
            type="button"
            role="menuitem"
            tabindex="-1"
            class="tree-menu-item text-danger"
            :disabled="menuFor.path === skillManifest"
            @click="act(() => emit('remove', menuFor!.path, false))"
          >
            <Icon :name="Trash2" :size="15" />Delete file<kbd>Del</kbd>
          </button>
        </template>
      </template>
    </div>
  </div>
</template>

<style scoped>
.tree-menu-item {
  display: flex;
  width: 100%;
  min-height: 36px;
  align-items: center;
  gap: 10px;
  border-radius: 8px;
  padding: 0 10px;
  text-align: left;
  font-size: 13px;
}
.tree-menu-item:hover:not(:disabled),
.tree-menu-item:focus-visible {
  background: var(--color-hover);
  outline: none;
}
.tree-menu-item.text-danger:hover:not(:disabled),
.tree-menu-item.text-danger:focus-visible {
  background: var(--color-danger-surface);
}
.tree-menu-item kbd {
  margin-left: auto;
  font-family: var(--font-mono);
  font-size: 10px;
  color: var(--color-subtle);
}
@media (max-width: 640px) {
  .tree-menu-item { min-height: 44px; }
}
</style>
