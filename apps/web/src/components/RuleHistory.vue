<script setup lang="ts">
import type { HookRuleCatalog } from '../../../../packages/contracts/generated/typescript/HookRuleCatalog'
import type { HookRuleDefinition } from '../../../../packages/contracts/generated/typescript/HookRuleDefinition'
import type { HookRuleTriggering } from '../../../../packages/contracts/generated/typescript/HookRuleTriggering'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { getHookRuleTriggering } from '../../../../packages/contracts/generated/getHookRuleTriggering'
import { listHookRuleTriggerings } from '../../../../packages/contracts/generated/listHookRuleTriggerings'
import { api } from '../api'
import { relativeTime, statusTitles, statusTone } from '../hook-rules'
import { ChevronDown, ChevronRight, Lock } from '../icons'
import Icon from './Icon.vue'
import RuleFlow from './RuleFlow.vue'

// Every member sees each triggering's state; its event and actions show only to those
// the installation allows.
const props = defineProps<{
  ruleId: string
  // The current version's tree, shown with the steps that ran when the triggering kept it.
  version: number
  definition: HookRuleDefinition
  catalog: HookRuleCatalog | null
  agentName: (id: string) => string
}>()

const triggerings = ref<HookRuleTriggering[]>([])
const withoutEffect = ref(0)
const notAdmitted = ref(0)
let generation = 0
let disposed = false
const next = ref<string | null>(null)
const loading = ref(true)
const error = ref('')
const open = ref<string | null>(null)
const detail = ref<HookRuleTriggering | null>(null)
const now = ref(Date.now())
const paged = ref(false)
const paging = ref(false)
let timer: ReturnType<typeof setInterval>

async function load(more = false) {
  if (paging.value)
    return
  if (more)
    paging.value = true
  const asked = ++generation
  const ruleId = props.ruleId
  try {
    const output = await listHookRuleTriggerings(api, { ruleId: props.ruleId, cursor: more ? next.value : null })
    if (disposed || asked !== generation || props.ruleId !== ruleId)
      return
    if (more)
      paged.value = true
    triggerings.value = more ? [...triggerings.value, ...output.triggerings] : output.triggerings
    withoutEffect.value = output.withoutEffect
    notAdmitted.value = output.notAdmitted ?? 0
    next.value = output.nextCursor ?? null
    error.value = ''
  }
  catch (e) {
    if (!disposed && asked === generation)
      error.value = (e as Error).message
  }
  finally {
    if (asked === generation) {
      loading.value = false
      paging.value = false
    }
  }
}

async function loadDetail(id: string) {
  try {
    const loaded = await getHookRuleTriggering(api, { triggeringId: id })
    if (!disposed && open.value === id) {
      detail.value = loaded
      triggerings.value = triggerings.value.map(entry => entry.id === id ? loaded : entry)
      error.value = ''
    }
  }
  catch (e) {
    if (!disposed && open.value === id)
      error.value = (e as Error).message
  }
}

async function toggle(triggering: HookRuleTriggering) {
  if (open.value === triggering.id) {
    open.value = null
    return
  }

  open.value = triggering.id
  detail.value = null
  await loadDetail(triggering.id)
}

const ran = computed(() => Object.fromEntries((detail.value?.steps ?? []).map(step => [step.nodeId, step.status])))
const migratedConversation = computed(() => detail.value?.event?.payload?.migratedConversation)
const tones: Record<string, string> = {
  success: 'bg-success',
  danger: 'bg-coral',
  muted: 'bg-line',
  active: 'bg-accent',
}

watch(() => props.ruleId, () => {
  loading.value = true
  open.value = null
  paged.value = false
  paging.value = false
  void load()
})
onMounted(() => {
  void load()
  timer = setInterval(() => {
    now.value = Date.now()
    if (!document.hidden) {
      if (open.value && detail.value && ['running', 'waiting', 'retrying'].includes(detail.value.status))
        void loadDetail(open.value)
      else if (!open.value && !paged.value && !paging.value)
        void load()
    }
  }, 5000)
})
onBeforeUnmount(() => {
  disposed = true
  generation++
  clearInterval(timer)
})

const pretty = (value: unknown) => JSON.stringify(value, null, 2)
</script>

<template>
  <section class="grid gap-3" aria-label="History">
    <p v-if="withoutEffect" class="m-0! text-xs text-muted">
      {{ withoutEffect }} {{ withoutEffect === 1 ? 'event' : 'events' }} in the last 24 hours ended without any action. They are not kept and do not count against the limits.
    </p>
    <p v-if="notAdmitted" class="m-0! rounded-xl bg-coral/8 px-3 py-2 text-xs text-coral">
      {{ notAdmitted }} {{ notAdmitted === 1 ? 'event was' : 'events were' }} not admitted in the last 24 hours: pending capacity or the admission deadline was reached. Other rules kept receiving their events.
    </p>
    <p v-if="error" class="m-0! text-sm text-coral" role="alert">
      {{ error }}
    </p>
    <p v-if="loading" class="m-0! text-sm text-muted" role="status">
      Loading the history…
    </p>
    <p v-else-if="!triggerings.length" class="m-0! rounded-2xl border border-dashed border-line p-5 text-center text-sm text-muted">
      No triggering has acted yet. They appear here as events arrive.
    </p>
    <ol class="m-0! grid list-none gap-2 p-0!">
      <li v-for="triggering in triggerings" :key="triggering.id" class="rounded-2xl border border-line bg-raised">
        <button
          type="button"
          class="flex w-full items-center gap-3 px-3.5 py-3 text-left"
          :aria-expanded="open === triggering.id"
          @click="toggle(triggering)"
        >
          <span class="size-2.5 shrink-0 rounded-full" :class="[tones[statusTone(triggering.status)], triggering.status === 'running' ? 'animate-pulse' : '']" />
          <span class="min-w-0 flex-1">
            <span class="block text-sm font-semibold">{{ statusTitles[triggering.status] }}<span class="font-normal text-muted"> · {{ triggering.actions }} {{ triggering.actions === 1 ? 'action' : 'actions' }} · version {{ triggering.version }}</span></span>
            <span v-if="triggering.error" class="block truncate text-xs text-coral">{{ triggering.error }}</span>
            <span v-else-if="triggering.resumesAt && triggering.status !== 'completed'" class="block text-xs text-muted">Resumes {{ new Date(triggering.resumesAt).toLocaleString() }}</span>
          </span>
          <span class="shrink-0 text-xs text-muted">{{ relativeTime(triggering.createdAt, now) }}</span>
          <Icon :name="open === triggering.id ? ChevronDown : ChevronRight" :size="16" class="text-muted" />
        </button>
        <div v-if="open === triggering.id" class="grid gap-3 border-t border-line p-3.5">
          <p v-if="!detail" class="m-0! text-sm text-muted">
            Loading…
          </p>
          <p v-else-if="!detail.detailsVisible" class="m-0! flex items-center gap-2 text-sm text-muted">
            <Icon :name="Lock" :size="15" />Its event and actions are visible to the rule’s creator, the owner and the members who can use its execution agent.
          </p>
          <template v-else>
            <RuleFlow
              v-if="!migratedConversation && detail.version === version"
              :nodes="definition.steps"
              container="/steps"
              :catalog="catalog"
              :agent-name="agentName"
              :ran="ran"
            />
            <RouterLink
              v-if="migratedConversation?.slug"
              :to="`/chats/${encodeURIComponent(migratedConversation.slug)}`"
              class="text-sm font-semibold text-accent hover:underline"
            >
              Open conversation · {{ migratedConversation.title }}
            </RouterLink>
            <section v-for="(step, index) in detail.steps ?? []" :key="`${step.nodeId}:${index}`" class="rounded-xl border border-line bg-surface p-3">
              <p class="m-0! flex items-center gap-2 text-sm font-semibold">
                <span class="size-2 rounded-full" :class="step.status === 'completed' ? 'bg-success' : step.status === 'failed' ? 'bg-coral' : 'bg-accent'" />
                {{ step.label || step.nodeId }}<span class="font-normal text-muted">· {{ step.kind }}{{ step.attempts > 1 ? ` · ${step.attempts} attempts` : '' }}</span>
              </p>
              <p v-if="step.error" class="m-0! mt-1! text-xs text-coral">
                {{ step.error }}
              </p>
              <RouterLink
                v-if="step.kind === 'startAgent' && step.status === 'completed' && typeof step.result?.conversationId === 'string'"
                :to="`/chats/${encodeURIComponent(step.result.conversationId)}`"
                class="mt-2 inline-block text-sm font-semibold text-accent hover:underline"
              >
                Open conversation
              </RouterLink>
              <details v-if="step.input !== undefined && step.input !== null" class="mt-2">
                <summary class="cursor-pointer text-xs font-semibold text-muted">
                  Input
                </summary>
                <pre class="mt-1 max-h-60 overflow-auto rounded-lg bg-canvas p-2.5 text-2xs">{{ pretty(step.input) }}</pre>
              </details>
              <details v-if="step.result !== undefined && step.result !== null" class="mt-2">
                <summary class="cursor-pointer text-xs font-semibold text-muted">
                  Result
                </summary>
                <pre class="mt-1 max-h-60 overflow-auto rounded-lg bg-canvas p-2.5 text-2xs">{{ pretty(step.result) }}</pre>
              </details>
            </section>
            <details v-if="detail.event">
              <summary class="cursor-pointer text-xs font-semibold text-muted">
                Event · {{ detail.event.hook }} · {{ new Date(detail.event.receivedAt).toLocaleString() }}
              </summary>
              <pre class="mt-1 max-h-72 overflow-auto rounded-lg bg-canvas p-2.5 text-2xs">{{ pretty(detail.event.payload) }}</pre>
            </details>
          </template>
        </div>
      </li>
    </ol>
    <button
      v-if="next"
      type="button"
      class="justify-self-center rounded-full border border-line px-4 py-2 text-sm font-semibold hover:bg-hover"
      @click="load(true)"
    >
      Show older triggerings
    </button>
  </section>
</template>
