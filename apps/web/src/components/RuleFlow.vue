<script setup lang="ts">
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleProblem } from '../../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { RuleNode } from '../../../../packages/contracts/generated/typescript/RuleNode'
import type { StepKind } from '../hook-rules'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import {
  describeAction,
  describeCondition,
  describeValue,
  problemsUnder,
  stepChoice,
  stepChoices,
} from '../hook-rules'
import {
  ArrowDown,
  ArrowUp,
  CircleAlert,
  Plus,
  Repeat,
  Split,
  Trash2,
} from '../icons'
import { ruleGraphWidth as flowWidth } from '../rule-graph'
import Icon from './Icon.vue'
import MenuButton from './MenuButton.vue'
import RuleFlowLink from './RuleFlowLink.vue'

defineOptions({ name: 'RuleFlow' })
// The persisted tree is presented as connected nodes. Branches rejoin before the
// next sibling; the graph never changes the execution order or invents new edges.
const props = defineProps<{
  nodes: RuleNode[]
  container: string
  catalog: HookRuleCatalog | null
  agentName: (id: string) => string
  editable?: boolean
  depth?: number
  inLoop?: boolean
  executionAgentId?: string
  selected?: string | null
  problems?: HookRuleProblem[]
  // Steps that ran in a triggering, for the history view.
  ran?: Record<string, 'completed' | 'failed' | 'running' | 'retrying' | 'waiting'>
}>()
const emit = defineEmits<{
  select: [path: string]
  insert: [container: string, index: number, kind: StepKind]
  remove: [path: string]
  move: [path: string, offset: -1 | 1]
}>()
const manualZoom = ref<number | null>(null)
const viewportWidth = ref(0)
const viewport = ref<HTMLElement | null>(null)
const root = computed(() => props.container === '/steps')
const width = computed(() => flowWidth(props.nodes))

const zoom = computed({
  get: () => manualZoom.value ?? Math.max(0.25, Math.min(1, ((viewportWidth.value || width.value + 48) - 48) / width.value)),
  set: value => manualZoom.value = value,
})
let resizeObserver: ResizeObserver | undefined
onMounted(() => {
  if (!root.value || !viewport.value)
    return
  resizeObserver = new ResizeObserver(() => viewportWidth.value = viewport.value?.clientWidth ?? 0)
  resizeObserver.observe(viewport.value)
})
onBeforeUnmount(() => resizeObserver?.disconnect())

function fit() {
  manualZoom.value = null
}

const choices = computed(() => stepChoices.filter((choice) => {
  if (choice.kind === 'wait' && props.inLoop)
    return false
  if (['if', 'forEach'].includes(choice.kind) && (props.depth ?? 0) >= 7)
    return false
  return choice.kind !== 'callTool' || props.catalog?.tools.some(tool => !props.executionAgentId || tool.agentIds.includes(props.executionAgentId))
}))

function hasProblems(path: string) {
  return problemsUnder(props.problems ?? [], path).length > 0
}

const tone: Record<string, string> = {
  callTool: 'bg-[#e8f1ff] text-[#2f5fb5] dark:bg-[#1d2a44] dark:text-[#8fb4ff]',
  startAgent: 'bg-[#efe9ff] text-[#5b44b8] dark:bg-[#2a2347] dark:text-[#b7a6ff]',
  steerConversation: 'bg-[#efe9ff] text-[#5b44b8] dark:bg-[#2a2347] dark:text-[#b7a6ff]',
  answerQuestion: 'bg-[#efe9ff] text-[#5b44b8] dark:bg-[#2a2347] dark:text-[#b7a6ff]',
  cancelConversation: 'bg-[#fdecea] text-[#b4412f] dark:bg-[#3d2320] dark:text-[#ff9f8f]',
  storeValue: 'bg-[#e9f7ef] text-[#277a4c] dark:bg-[#1c3427] dark:text-[#87d8a8]',
  wait: 'bg-[#fff4e0] text-[#9a6200] dark:bg-[#3a2c14] dark:text-[#ffc66b]',
  if: 'bg-[#fff0f6] text-[#a8336b] dark:bg-[#3a1d2b] dark:text-[#ff9cc8]',
  forEach: 'bg-[#e8f8fa] text-[#1d7b86] dark:bg-[#183236] dark:text-[#7fd6e0]',
}
const ranTone: Record<string, string> = {
  completed: 'ring-2 ring-success/60',
  failed: 'ring-2 ring-coral/70',
  running: 'ring-2 ring-accent/60',
  retrying: 'ring-2 ring-accent/60',
  waiting: 'ring-2 ring-accent/40',
}
</script>

<template>
  <div :class="root ? 'rule-graph' : 'subgraph'">
    <div v-if="root" class="graph-toolbar">
      <span>{{ editable ? 'Follow the arrows · select a node to edit' : 'Follow the arrows from the event to the end' }}</span>
      <div class="flex items-center gap-1">
        <button type="button" aria-label="Zoom out" @click="zoom = Math.max(0.25, zoom - 0.15)">
          −
        </button>
        <span class="min-w-10 text-center text-xs tabular-nums">{{ Math.round(zoom * 100) }}%</span>
        <button type="button" aria-label="Zoom in" @click="zoom = Math.min(1.5, zoom + 0.15)">
          +
        </button>
        <button type="button" class="px-2! text-xs!" @click="fit">
          Fit
        </button>
      </div>
    </div>
    <div ref="viewport" :class="root ? 'graph-viewport' : ''">
      <ol class="rule-flow mx-auto! my-0! grid list-none gap-0 p-0!" :style="{ width: `${width}px`, zoom: root ? zoom : undefined }" :aria-label="root ? 'Steps' : undefined">
        <li v-if="root" class="graph-terminal">
          Event received
        </li>
        <li v-if="root && !editable" aria-hidden="true">
          <RuleFlowLink />
        </li>
        <li v-if="editable" class="flow-gap insertion-gap relative flex items-center justify-center py-1.5">
          <MenuButton
            :label="nodes.length ? 'Add a step here' : 'Add the first step'"
            menu-label="Add a step"
            :width="320"
            trigger-class="add-step relative z-10 grid size-7 place-items-center rounded-full border border-dashed border-line bg-canvas text-muted hover:border-accent hover:text-accent"
          >
            <template #trigger>
              <Icon :name="Plus" :size="14" />
            </template>
            <template #default="{ select }">
              <button
                v-for="choice in choices"
                :key="choice.kind"
                type="button"
                role="menuitem"
                class="flex w-full items-start gap-3 rounded-lg px-2 py-2.5 text-left hover:bg-hover"
                @click="select(() => emit('insert', container, 0, choice.kind))"
              >
                <span class="mt-0.5 grid size-7 shrink-0 place-items-center rounded-lg" :class="tone[choice.kind]"><Icon :name="choice.icon" :size="15" /></span>
                <span class="min-w-0"><span class="block text-sm font-semibold">{{ choice.title }}</span><span class="block text-xs text-muted">{{ choice.description }}</span></span>
              </button>
            </template>
          </MenuButton>
        </li>
        <template v-for="(node, index) in nodes" :key="node.id">
          <li class="flow-node relative">
            <article
              class="node-card group relative mx-auto w-60 rounded-[18px] border bg-raised shadow-[0_2px_8px_rgb(0_0_0/0.04)] transition-colors"
              :class="[
                selected === `${container}/${index}` ? 'border-accent ring-3 ring-accent/15' : hasProblems(`${container}/${index}`) ? 'border-coral/70' : 'border-line',
                ran?.[node.id] ? ranTone[ran[node.id]] : '',
              ]"
            >
              <div class="relative">
                <button
                  v-if="editable"
                  type="button"
                  class="absolute inset-0 rounded-[18px] focus-visible:outline-2 focus-visible:outline-accent"
                  :aria-label="`Edit ${node.type === 'action' ? (node.label || describeAction(node.action, catalog, agentName).title) : node.id}`"
                  @click="emit('select', `${container}/${index}`)"
                />
                <div class="pointer-events-none relative flex items-start gap-3 p-3.5">
                  <span class="grid size-9 shrink-0 place-items-center rounded-xl" :class="tone[node.type === 'action' ? node.action.type : node.type]">
                    <Icon :name="node.type === 'action' ? stepChoice(node.action.type).icon : node.type === 'if' ? Split : Repeat" :size="18" />
                  </span>
                  <div class="min-w-0 flex-1">
                    <template v-if="node.type === 'action'">
                      <p class="m-0! line-clamp-2 font-heading text-[15px] font-semibold">
                        {{ node.label || describeAction(node.action, catalog, agentName).title }}
                      </p>
                      <p class="m-0! mt-0.5! line-clamp-2 text-xs text-muted">
                        {{ node.label ? describeAction(node.action, catalog, agentName).title : '' }}{{ node.label && describeAction(node.action, catalog, agentName).detail ? ' · ' : '' }}{{ describeAction(node.action, catalog, agentName).detail }}
                      </p>
                    </template>
                    <template v-else-if="node.type === 'if'">
                      <p class="m-0! font-heading text-[15px] font-semibold">
                        If
                      </p>
                      <p class="m-0! mt-0.5! line-clamp-3 text-xs text-muted">
                        {{ describeCondition(node.condition) }}
                      </p>
                    </template>
                    <template v-else>
                      <p class="m-0! font-heading text-[15px] font-semibold">
                        For each {{ node.itemName }}
                      </p>
                      <p class="m-0! mt-0.5! truncate text-xs text-muted">
                        in {{ describeValue(node.items) }}
                      </p>
                    </template>
                  </div>
                  <span v-if="hasProblems(`${container}/${index}`)" class="shrink-0 text-coral" :title="problemsUnder(problems ?? [], `${container}/${index}`)[0]?.message">
                    <Icon :name="CircleAlert" :size="17" />
                  </span>
                </div>
              </div>
              <div v-if="editable" class="node-tools absolute right-2 -top-8 z-10 flex w-fit gap-0.5 rounded-lg border border-line bg-raised p-0.5 opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
                <button
                  type="button"
                  class="grid size-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-ink disabled:opacity-30"
                  :disabled="index === 0"
                  aria-label="Move up"
                  @click.stop="emit('move', `${container}/${index}`, -1)"
                >
                  <Icon :name="ArrowUp" :size="14" />
                </button>
                <button
                  type="button"
                  class="grid size-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-ink disabled:opacity-30"
                  :disabled="index === nodes.length - 1"
                  aria-label="Move down"
                  @click.stop="emit('move', `${container}/${index}`, 1)"
                >
                  <Icon :name="ArrowDown" :size="14" />
                </button>
                <button
                  type="button"
                  class="grid size-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-coral"
                  aria-label="Remove this step"
                  @click.stop="emit('remove', `${container}/${index}`)"
                >
                  <Icon :name="Trash2" :size="14" />
                </button>
              </div>
            </article>
            <div v-if="node.type === 'if'" class="mx-auto" :style="{ width: `${flowWidth(node.then) + flowWidth(node.otherwise) + 64}px` }">
              <RuleFlowLink kind="split" :left-width="flowWidth(node.then)" :right-width="flowWidth(node.otherwise)" />
              <div class="branches relative grid gap-16" :style="{ gridTemplateColumns: `${flowWidth(node.then)}px ${flowWidth(node.otherwise)}px` }">
                <section class="graph-branch" aria-label="When it holds">
                  <p class="sr-only">
                    Then
                  </p>
                  <RuleFlow
                    :nodes="node.then"
                    :container="`${container}/${index}/then`"
                    :catalog="catalog"
                    :agent-name="agentName"
                    :in-loop="inLoop"
                    :depth="(depth ?? 0) + 1"
                    :editable="editable"
                    :execution-agent-id="executionAgentId"
                    :selected="selected"
                    :problems="problems"
                    :ran="ran"
                    @select="path => emit('select', path)"
                    @insert="(at, position, kind) => emit('insert', at, position, kind)"
                    @remove="path => emit('remove', path)"
                    @move="(path, offset) => emit('move', path, offset)"
                  />
                  <p v-if="!node.then.length && !editable" class="m-0! px-1 py-2 text-xs text-muted">
                    Nothing.
                  </p>
                </section>
                <section class="graph-branch" aria-label="Otherwise">
                  <p class="sr-only">
                    Otherwise
                  </p>
                  <RuleFlow
                    :nodes="node.otherwise"
                    :container="`${container}/${index}/otherwise`"
                    :catalog="catalog"
                    :agent-name="agentName"
                    :in-loop="inLoop"
                    :depth="(depth ?? 0) + 1"
                    :editable="editable"
                    :execution-agent-id="executionAgentId"
                    :selected="selected"
                    :problems="problems"
                    :ran="ran"
                    @select="path => emit('select', path)"
                    @insert="(at, position, kind) => emit('insert', at, position, kind)"
                    @remove="path => emit('remove', path)"
                    @move="(path, offset) => emit('move', path, offset)"
                  />
                  <p v-if="!node.otherwise.length && !editable" class="m-0! px-1 py-2 text-xs text-muted">
                    No action in this branch.
                  </p>
                </section>
              </div>
              <RuleFlowLink kind="merge" :left-width="flowWidth(node.then)" :right-width="flowWidth(node.otherwise)" />
            </div>
            <div v-else-if="node.type === 'forEach'" class="loop-graph" :style="{ width: `${flowWidth(node.body) + 96}px` }">
              <RuleFlowLink />
              <span class="loop-return" aria-hidden="true" /><span class="loop-return-label">Next item</span>
              <span class="loop-exit" aria-hidden="true" /><span class="loop-exit-label">When finished</span>
              <section class="mx-auto" :aria-label="`For each ${node.itemName}`">
                <RuleFlow
                  :nodes="node.body"
                  :container="`${container}/${index}/body`"
                  :catalog="catalog"
                  :agent-name="agentName"
                  :in-loop="true"
                  :depth="(depth ?? 0) + 1"
                  :editable="editable"
                  :execution-agent-id="executionAgentId"
                  :selected="selected"
                  :problems="problems"
                  :ran="ran"
                  @select="path => emit('select', path)"
                  @insert="(at, position, kind) => emit('insert', at, position, kind)"
                  @remove="path => emit('remove', path)"
                  @move="(path, offset) => emit('move', path, offset)"
                />
              </section>
            </div>
          </li>
          <li v-if="editable" class="flow-gap insertion-gap relative flex items-center justify-center py-1.5">
            <MenuButton
              label="Add a step here"
              menu-label="Add a step"
              :width="320"
              trigger-class="add-step relative z-10 grid size-7 place-items-center rounded-full border border-dashed border-line bg-canvas text-muted hover:border-accent hover:text-accent"
            >
              <template #trigger>
                <Icon :name="Plus" :size="14" />
              </template>
              <template #default="{ select }">
                <button
                  v-for="choice in choices"
                  :key="choice.kind"
                  type="button"
                  role="menuitem"
                  class="flex w-full items-start gap-3 rounded-lg px-2 py-2.5 text-left hover:bg-hover"
                  @click="select(() => emit('insert', container, index + 1, choice.kind))"
                >
                  <span class="mt-0.5 grid size-7 shrink-0 place-items-center rounded-lg" :class="tone[choice.kind]"><Icon :name="choice.icon" :size="15" /></span>
                  <span class="min-w-0"><span class="block text-sm font-semibold">{{ choice.title }}</span><span class="block text-xs text-muted">{{ choice.description }}</span></span>
                </button>
              </template>
            </MenuButton>
          </li>
          <li v-else-if="index < nodes.length - 1" class="flow-gap" aria-hidden="true">
            <RuleFlowLink />
          </li>
        </template>
        <li v-if="root" aria-hidden="true">
          <RuleFlowLink />
        </li>
        <li v-if="root" class="graph-terminal">
          End
        </li>
      </ol>
    </div>
  </div>
</template>

<style scoped>
.rule-graph { border: 1px solid var(--color-line); border-radius: 20px; overflow: hidden; background: var(--color-canvas); }
.graph-toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 14px; border-bottom: 1px solid var(--color-line); color: var(--color-muted); font-size: 12px; }
.graph-toolbar button { display: grid; place-items: center; min-width: 28px; height: 28px; border: 1px solid var(--color-line); border-radius: 8px; background: var(--color-raised); color: var(--color-ink); font-size: 18px; }
.graph-viewport {  min-height: 320px; overflow-x: auto; padding: 28px 24px; background-image: radial-gradient(var(--color-line) 1px, transparent 1px); background-size: 18px 18px; }
.rule-flow { position: relative; margin-inline: auto !important; }
.graph-terminal { justify-self: center; padding: 8px 18px; border: 1px solid var(--color-line); border-radius: 30px; background: var(--color-raised); color: var(--color-muted); font-size: 12px; font-weight: 600; }
.flow-node { min-width: 0; }.node-card { z-index: 1; }
.graph-branch { display: flex; flex-direction: column; align-items: center; }
.graph-branch::after { content: ''; flex: 1; min-height: 0; width: 1.6px; background: var(--color-muted); }
.insertion-gap { min-height: 56px; }
.insertion-gap::before { content: ''; position: absolute; top: 0; bottom: 0; left: 50%; width: 1.6px; transform: translateX(-50%); background: var(--color-muted); }
.insertion-gap::after { content: ''; position: absolute; bottom: 1px; left: 50%; width: 7px; height: 7px; border-right: 2px solid var(--color-muted); border-bottom: 2px solid var(--color-muted); transform: translateX(-50%) rotate(45deg); }
.flow-gap > :deep(.add-step) { background: var(--color-raised); }
.loop-graph { position: relative; margin-inline: auto; padding: 0 48px 64px; }
.loop-return, .loop-exit { position: absolute; z-index: 0; width: calc(50% - 8px); top: -56px; border: 1.6px solid var(--color-muted); pointer-events: none; }
.loop-return { right: 8px; bottom: 64px; border-left: 0; border-radius: 0 20px 20px 0; }
.loop-exit { left: 8px; bottom: 0; border-right: 0; border-radius: 20px 0 0 20px; }
.loop-return-label, .loop-exit-label { position: absolute; top: 50%; padding: 4px; background: var(--color-canvas); font-size: 10px; color: var(--color-muted); writing-mode: vertical-rl; }
.loop-return::before { content: ''; position: absolute; top: -4px; left: 116px; width: 8px; height: 8px; border-left: 2px solid var(--color-muted); border-bottom: 2px solid var(--color-muted); transform: rotate(45deg); }
.loop-return-label { right: 1px; }.loop-exit-label { left: 1px; }
</style>
