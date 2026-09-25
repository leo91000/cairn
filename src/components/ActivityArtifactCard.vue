<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { computed, ref, useId } from 'vue'
import {
  BookOpen,
  Check,
  ChevronDown,
  CircleAlert,
  Clock,
  FileCode,
  FolderSearch,
  Globe,
  Info,
  ListChecks,
  LoaderCircle,
  Search,
  Sparkles,
  Terminal,
  Wrench,
} from '../icons'
import ActivityCode from './ActivityCode.vue'
import ActivityContent from './ActivityContent.vue'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'

// `bare` renders only the facts and detail, for a sheet that already shows the step's heading.
const props = defineProps<{
  artifact: ActivityArtifact
  active: boolean
  expanded: boolean
  bare?: boolean
}>()
defineEmits<{ toggle: [] }>()
const id = useId()
const raw = ref(false)
const commandOpen = ref(false)
const preview = ref(true)

function markdownParts(content: string) {
  const match = content.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/)
  return { metadata: match?.[1] ?? '', body: match ? content.slice(match[0].length) : content }
}

const icons = {
  command: Terminal,
  read: BookOpen,
  browse: FolderSearch,
  output: Terminal,
  files: FileCode,
  search: Search,
  tool: Wrench,
  plan: ListChecks,
  thinking: Sparkles,
  notice: Info,
}
const labels = {
  command: 'Terminal',
  read: 'File read',
  browse: 'Workspace',
  output: 'Output',
  files: 'File edit',
  search: 'Search',
  tool: 'Tool call',
  plan: 'Plan',
  thinking: 'Thinking',
  notice: 'Session',
}
const running = computed(() => props.artifact.status === 'running' && props.active)
const status = computed(() => props.artifact.statusLabel ?? (running.value ? 'Running' : props.artifact.status === 'running' ? 'Stopped' : props.artifact.status === 'error' ? 'Failed' : props.artifact.historical ? 'Recorded' : props.artifact.status === 'info' ? 'Info' : 'Completed'))
const blocks = computed(() => props.artifact.blocks.filter(block => block.label !== 'Command'))
const outputLines = computed(() => blocks.value.reduce((total, block) => total + (block.code ? block.code.trimEnd().split('\n').length : 0), 0))
const diff = computed(() => {
  let added = 0
  let removed = 0
  for (const block of props.artifact.blocks.filter(block => block.language === 'diff')) {
    for (const line of block.code.split('\n')) {
      if (line.startsWith('+') && !line.startsWith('+++'))
        added++
      if (line.startsWith('-') && !line.startsWith('---'))
        removed++
    }
  }

  return { added, removed }
})
const elapsed = computed(() => {
  const ms = props.artifact.durationMs
  if (ms === undefined)
    return ''
  if (ms < 1000)
    return `${ms} ms`
  return ms < 60000 ? `${(ms / 1000).toFixed(1)} s` : `${Math.floor(ms / 60000)}m ${Math.floor(ms % 60000 / 1000)}s`
})
</script>

<template>
  <article class="activity-artifact operation-card" :data-kind="artifact.kind" :data-status="artifact.status">
    <button
      v-if="!bare"
      class="artifact-toggle flex items-center gap-3 w-full border-0 bg-transparent text-left text-muted cursor-pointer phone:gap-2 px-0 py-4"
      :aria-expanded="expanded"
      :aria-controls="id"
      @click="$emit('toggle')"
    >
      <span class="operation-icon grid place-items-center w-10 h-10 border border-line rounded-xl bg-surface text-muted phone:w-8 phone:h-8 phone:rounded-[10px]"><Icon v-if="artifact.kind === 'search' && !artifact.command" :name="Globe" :size="19" /><Icon v-else :name="icons[artifact.kind]" :size="19" /></span>
      <span class="artifact-heading flex-1 min-w-0"><span class="operation-label">{{ labels[artifact.kind] }}</span><strong>{{ artifact.title }}</strong><span v-if="artifact.subtitle" class="operation-description" :class="{ 'artifact-command': artifact.command || artifact.kind === 'read' }">{{ artifact.subtitle }}</span></span>
      <span class="operation-state inline-flex items-center gap-[5px] mt-0.5 rounded-[5px] text-micro whitespace-nowrap text-muted bg-surface phone:[grid-column:2] phone:[grid-row:2] phone:[justify-self:start] px-[7px] py-1 phone:m-0" :data-state="status"><Icon
        v-if="running"
        :name="LoaderCircle"
        class="activity-spinning [animation:activity-spin_1.5s_linear_infinite] [@media(prefers-reduced-motion:_reduce)]:[animation:none]"
        :size="13"
      /><Icon v-else-if="artifact.status === 'error'" :name="CircleAlert" :size="13" /><Icon v-else-if="status === 'Completed'" :name="Check" :size="13" /><span>{{ status }}</span></span>
      <Icon
        :name="ChevronDown"
        class="artifact-chevron [transition:transform_.18s] shrink-0"
        :class="{ rotated: expanded }"
        :size="15"
      />
    </button>
    <div class="operation-facts flex items-center flex-wrap gap-[8px_14px] text-3xs text-muted phone:gap-[6px_10px]" :class="bare ? 'mb-2' : '-mt-0.5 mr-4 mb-3.5 ml-17 phone:mt-0 phone:mr-3 phone:mb-3 phone:ml-[53px]'">
      <span v-if="elapsed"><Icon :name="Clock" :size="12" />{{ elapsed }}</span>
      <span v-if="artifact.exitCode !== undefined" :class="{ 'operation-error': artifact.status === 'error' }">Exit {{ artifact.exitCode }}</span>
      <span v-if="artifact.files.length">{{ artifact.files.length }} {{ artifact.files.length === 1 ? 'file' : 'files' }}</span>
      <span v-if="diff.added || diff.removed" class="operation-diff"><b>+{{ diff.added }}</b><b>−{{ diff.removed }}</b></span>
      <span v-if="outputLines">{{ outputLines.toLocaleString() }} {{ outputLines === 1 ? 'line' : 'lines' }}</span>
      <span v-if="artifact.cwd" class="operation-cwd">{{ artifact.cwd }}</span>
    </div>
    <div
      v-if="expanded"
      :id="id"
      class="artifact-body pt-0 pr-0 pb-4 min-w-0"
      :class="bare ? 'pl-0' : 'pl-[29px] phone:pl-0'"
    >
      <ul v-if="artifact.files.length" class="artifact-files [list-style:none] mt-2 mb-[15px] p-0 mx-0">
        <li v-for="(file, index) in artifact.files" :key="`${file.path}:${index}`">
          <Icon :name="FileCode" :size="16" /><code>{{ file.path }}</code><span :data-change="file.kind">{{ file.kind }}</span>
        </li>
      </ul>
      <ul v-if="artifact.tasks.length" class="artifact-plan [list-style:none] mt-2 mb-[15px] p-0 mx-0">
        <li v-for="(task, index) in artifact.tasks" :key="index" :class="{ completed: task.completed }">
          <span><Icon v-if="task.completed" :name="Check" :size="13" /></span>{{ task.text }}
        </li>
      </ul>
      <template v-if="artifact.command">
        <ActivityCode
          v-if="artifact.kind === 'command' || commandOpen"
          label="Command"
          :code="artifact.command"
          language="bash"
        />
        <button
          v-if="artifact.kind !== 'command'"
          class="operation-command-toggle flex items-center gap-1.5 border-0 bg-transparent pt-2.5 pb-[3px] text-3xs text-muted cursor-pointer phone:min-h-11 px-0"
          :aria-expanded="commandOpen"
          @click="commandOpen = !commandOpen"
        >
          <Icon :name="Terminal" :size="13" />{{ commandOpen ? 'Hide' : 'Show' }} command
        </button>
      </template>
      <template v-for="(block, index) in blocks" :key="index">
        <div v-if="artifact.kind === 'read' && block.language === 'markdown'" class="read-preview border border-line rounded-[9px] mt-3 overflow-hidden">
          <div class="read-preview-tabs flex gap-[5px] bg-surface [border-bottom:1px_solid_light-dark(#dad9e7,_var(--dark-border))] px-2 py-1.5">
            <button :aria-pressed="preview" @click="preview = true">
              Preview file
            </button><button :aria-pressed="!preview" @click="preview = false">
              View source
            </button>
          </div>
          <div v-if="preview" class="read-preview-content overflow-visible text-xs leading-[1.8] [scrollbar-width:thin] wrap-anywhere p-[15px]">
            <details v-if="markdownParts(block.code).metadata" class="read-metadata">
              <summary>File metadata</summary><ActivityCode label="Metadata" :code="markdownParts(block.code).metadata" language="yaml" />
            </details>
            <Markdown :content="markdownParts(block.code).body" />
          </div>
          <ActivityCode
            v-else
            :label="block.label"
            :code="block.code"
            language="markdown"
          />
        </div>
        <ActivityContent
          v-else
          :label="block.label"
          :content="block.code"
          :language="block.language"
        />
      </template>
      <p v-if="!blocks.length && !artifact.files.length && !artifact.tasks.length" class="artifact-no-output text-2xs text-muted leading-[1.8]">
        {{ running ? 'Waiting for output…' : 'No output for this step.' }}
      </p>
      <p v-if="artifact.historical" class="operation-history flex items-start gap-[7px] text-3xs text-muted leading-[1.7] mt-3 mb-0 mx-0">
        <Icon :name="Info" :size="13" />This older step saved output only. Its command and exit code weren’t recorded.
      </p>
      <button class="artifact-raw-toggle text-3xs text-muted border-0 bg-transparent cursor-pointer [text-decoration:underline] [text-underline-offset:3px] phone:min-h-11 px-0 py-2" :aria-expanded="raw" @click="raw = !raw">
        {{ raw ? 'Hide' : 'View' }} event details
      </button>
      <ActivityCode
        v-if="raw"
        label="Event details"
        :code="artifact.raw"
        :language="artifact.historical ? 'plaintext' : 'json'"
      />
    </div>
  </article>
</template>
