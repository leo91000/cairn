<script setup lang="ts">
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleProblem } from '../../../../packages/contracts/generated/typescript/HookRuleProblem'
import type { RuleNode } from '../../../../packages/contracts/generated/typescript/RuleNode'
import type { StepKind } from '../hook-rules'
import { computed } from 'vue'
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
import Icon from './Icon.vue'
import MenuButton from './MenuButton.vue'

defineOptions({ name: 'RuleFlow' })
// The tree as a vertical flow: each block is a card, a choice opens two lanes and a loop
// one indented lane. Editing inserts, selects, moves and removes blocks by path.
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
  <ol class="rule-flow m-0! grid list-none gap-0 p-0!" :aria-label="container === '/steps' ? 'Steps' : undefined">
    <li v-if="editable" class="flow-gap relative flex justify-center py-1.5">
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
          class="node-card group relative rounded-[18px] border bg-raised shadow-[0_1px_0_rgb(0_0_0/0.03)] transition-colors"
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
          <div v-if="editable" class="node-tools relative z-10 mr-2 mb-2 ml-auto flex w-fit gap-0.5 rounded-lg border border-line bg-raised p-0.5">
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
          <div v-if="node.type === 'if'" class="branches relative grid grid-cols-2 gap-3 border-t border-line p-3 tablet:grid-cols-1">
            <section class="lane rounded-2xl bg-success/6 p-2.5" aria-label="When it holds">
              <p class="m-0! mb-1! px-1 text-2xs font-bold tracking-wide text-success uppercase">
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
            <section class="lane rounded-2xl bg-ink/4 p-2.5" aria-label="Otherwise">
              <p class="m-0! mb-1! px-1 text-2xs font-bold tracking-wide text-muted uppercase">
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
                Nothing: the triggering ends.
              </p>
            </section>
          </div>
          <div v-else-if="node.type === 'forEach'" class="relative border-t border-line p-3">
            <section class="lane rounded-2xl border-l-3 border-[#1d7b86]/40 bg-[#1d7b86]/5 p-2.5" :aria-label="`For each ${node.itemName}`">
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
        </article>
      </li>
      <li v-if="editable" class="flow-gap relative flex justify-center py-1.5">
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
      <li v-else-if="index < nodes.length - 1" class="flow-gap h-4" aria-hidden="true" />
    </template>
  </ol>
</template>

<style scoped>
/* The connector runs behind the cards and the add buttons. */
.rule-flow {
  position: relative;
}
.flow-gap::before {
  content: '';
  position: absolute;
  top: 0;
  bottom: 0;
  left: 50%;
  width: 2px;
  transform: translateX(-1px);
  background: var(--color-line, #e4e2ee);
}
.flow-gap:first-child::before {
  top: 50%;
}
.flow-gap:last-child::before {
  bottom: 50%;
}
</style>
