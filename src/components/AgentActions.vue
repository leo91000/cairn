<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { computed, ref, useId } from 'vue'
import { actionSentence, agentSteps } from '../agent-steps'
import { Check, ChevronDown, ChevronRight, Clock, FileText, Folder, Pencil, Plug, Search, Sparkles, Terminal } from '../icons'
import ActivityCode from './ActivityCode.vue'
import ActivityContent from './ActivityContent.vue'
import Icon from './Icon.vue'
import Modal from './Modal.vue'

// The agent's actions between two messages, as on Android: a one-sentence summary that expands
// into a timeline; each step opens its full detail. While the agent works, the step in progress
// is left to the working indicator below.
const props = defineProps<{ artifacts: ActivityArtifact[], active: boolean }>()
const expanded = ref(false)
const opened = ref<number | null>(null)
const timelineId = useId()
const running = (artifact: ActivityArtifact) => artifact.status === 'running' && props.active
const shown = computed(() => props.artifacts.filter(artifact => !running(artifact)))
const steps = computed(() => agentSteps(shown.value))
const sentence = computed(() => actionSentence(shown.value))
const failures = computed(() => steps.value.filter(step => step.failed).length)
const kinds = computed(() => [...new Set(shown.value.map(artifact => artifact.kind))].filter(kind => kind !== 'notice').slice(0, 4))
const glyphs = { command: Terminal, output: Terminal, read: FileText, files: Pencil, browse: Folder, search: Search, plan: Check, thinking: Sparkles, tool: Plug, notice: Clock }
const openedStep = computed(() => opened.value === null ? undefined : steps.value[opened.value])
function status(step: (typeof steps.value)[number]) {
  if (step.failed) {
    const code = step.items.find(item => item.exitCode !== undefined && item.exitCode !== 0)?.exitCode
    return code === undefined ? 'Failed' : `Exit ${code}`
  }
  return step.running ? 'running' : ''
}
</script>

<template>
  <section v-if="steps.length" class="agent-actions my-3 overflow-hidden rounded-[18px]" :class="expanded ? 'border border-line bg-surface' : 'bg-variant'" data-testid="agent-actions">
    <button class="flex min-h-12 w-full items-center gap-2.5 px-3 py-2.5 text-left" :aria-expanded="expanded" :aria-controls="timelineId" @click="expanded = !expanded">
      <span v-if="!expanded" class="flex shrink-0 -space-x-2" aria-hidden="true">
        <span v-for="kind in kinds" :key="kind" class="grid size-6.5 place-items-center rounded-full border-2 border-variant bg-surface text-muted"><Icon :name="glyphs[kind]" :size="13" /></span>
      </span>
      <span class="min-w-0 flex-1">
        <span class="block" :class="expanded ? 'truncate text-xs text-muted' : 'line-clamp-2 text-base text-ink'">{{ sentence }}</span>
        <span v-if="!expanded" class="flex items-center gap-1.5 text-2xs text-muted">
          {{ steps.length === 1 ? '1 step' : `${steps.length} steps` }}
          <template v-if="failures">
            <span aria-hidden="true">·</span><span class="inline-flex items-center gap-1 text-coral"><span class="size-1.5 rounded-full bg-coral" />{{ failures === 1 ? '1 failure' : `${failures} failures` }}</span>
          </template>
        </span>
      </span>
      <Icon :name="ChevronDown" :size="18" class="shrink-0 text-muted transition-transform" :class="expanded ? 'rotate-180' : ''" />
    </button>
    <ol v-if="expanded" :id="timelineId" class="m-0! list-none pr-2.5 pb-3 pl-3.5">
      <li v-for="(step, index) in steps" :key="step.id">
        <button class="flex w-full rounded-xl text-left hover:bg-hover/60" data-testid="agent-step" :aria-label="[step.title, status(step), step.detail].filter(Boolean).join(', ')" @click="opened = index">
          <span class="flex shrink-0 flex-col items-center self-stretch">
            <span class="grid size-6.5 shrink-0 place-items-center rounded-full" :class="step.failed ? 'bg-coral-soft text-coral' : step.running ? 'bg-accent text-surface' : 'bg-variant text-muted'"><Icon :name="glyphs[step.items[0]!.kind]" :size="13" /></span>
            <span v-if="index < steps.length - 1" class="w-0.5 flex-1 bg-line" />
          </span>
          <span class="ml-3 min-w-0 flex-1 pt-0.75" :class="index < steps.length - 1 ? 'pb-3' : 'pb-0.5'">
            <span class="flex items-center gap-2">
              <span class="min-w-0 flex-1 truncate text-base font-semibold text-ink">{{ step.title }}</span>
              <span v-if="status(step)" class="shrink-0 text-2xs font-bold" :class="step.failed ? 'text-coral' : 'text-accent'">{{ status(step) }}</span>
              <Icon v-else :name="ChevronRight" :size="14" class="shrink-0 text-muted/60" />
            </span>
            <span v-if="step.detail" class="block truncate text-2xs text-muted" :class="step.items[0]!.kind === 'thinking' || step.items[0]!.kind === 'notice' ? '' : 'font-mono'">{{ step.detail }}</span>
          </span>
        </button>
      </li>
    </ol>
  </section>
  <Modal v-if="openedStep" :title="openedStep.title" sheet wide @close="opened = null">
    <div class="grid gap-3 px-5 pt-4 pb-6 phone:px-4" data-testid="agent-step-sheet">
      <div class="flex items-center gap-3">
        <span class="grid size-10 shrink-0 place-items-center rounded-xl" :class="openedStep.failed ? 'bg-coral-soft text-coral' : openedStep.running ? 'bg-soft text-accent' : 'bg-variant text-muted'"><Icon :name="glyphs[openedStep.items[0]!.kind]" :size="20" /></span>
        <p class="m-0! text-xs" :class="openedStep.failed ? 'text-coral' : openedStep.running ? 'text-accent' : 'text-muted'">
          {{ [openedStep.failed ? 'Failed' : openedStep.running ? 'Running' : 'Completed', openedStep.failed ? status(openedStep).replace('Failed', '') : '', `step ${opened! + 1} of ${steps.length}`].filter(Boolean).join(' · ') }}
        </p>
      </div>
      <template v-for="(artifact, index) in openedStep.items" :key="artifact.id">
        <hr v-if="index" class="border-line">
        <h3 v-if="openedStep.items.length > 1" class="m-0! text-base font-semibold">
          {{ artifact.title }}
        </h3>
        <p v-if="artifact.kind === 'notice' && artifact.subtitle" class="m-0! text-sm text-muted">
          {{ artifact.subtitle }}
        </p>
        <ul v-if="artifact.files.length && artifact.kind !== 'read'" class="m-0! grid list-none gap-1 p-0!">
          <li v-for="(file, fileIndex) in artifact.files" :key="`${file.path}:${fileIndex}`" class="flex items-center gap-2 text-xs">
            <Icon :name="Pencil" :size="14" class="text-muted" /><code class="min-w-0 flex-1 truncate">{{ file.path }}</code><span class="text-2xs text-muted">{{ file.kind }}</span>
          </li>
        </ul>
        <ul v-if="artifact.tasks.length" class="m-0! grid list-none gap-1 p-0!">
          <li v-for="(task, taskIndex) in artifact.tasks" :key="taskIndex" class="flex items-start gap-2 text-sm" :class="task.completed ? 'text-muted line-through' : ''">
            <Icon :name="Check" :size="14" class="mt-0.5" :class="task.completed ? 'text-accent' : 'opacity-0'" />{{ task.text }}
          </li>
        </ul>
        <ActivityCode v-if="artifact.command" label="Command" :code="artifact.command" language="bash" />
        <ActivityContent v-for="(block, blockIndex) in artifact.blocks.filter(block => block.label !== 'Command')" :key="blockIndex" :label="block.label" :content="block.code" :language="block.language" />
        <p v-if="!artifact.command && !artifact.blocks.length && !artifact.files.length && !artifact.tasks.length" class="m-0! text-xs text-muted">
          {{ artifact.status === 'running' ? 'Waiting for output…' : 'No output for this step.' }}
        </p>
        <details class="text-xs text-muted">
          <summary class="min-h-9 cursor-pointer py-2">
            Technical details · JSON / source
          </summary>
          <ActivityCode label="Event details" :code="artifact.raw" :language="artifact.historical ? 'plaintext' : 'json'" />
        </details>
      </template>
    </div>
  </Modal>
</template>
