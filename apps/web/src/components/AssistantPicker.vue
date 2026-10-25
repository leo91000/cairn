<script setup lang="ts">
import type { CSSProperties } from 'vue'
import type { Provider } from '../../../../packages/contracts/accounts'
import type { CodingAgentModel } from '../../../../packages/contracts/models'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  useId,
  watch,
} from 'vue'
import { providers as codingAgents } from '../../../../packages/contracts/accounts'
import {
  billingLabels,
  effortLabel,
  speedLabel,
  speedUsageUrl,
  STANDARD_SPEED,
} from '../../../../packages/contracts/models'
import { brands } from '../accounts'
import {
  Check,
  ChevronDown,
  Gauge,
  Info,
  LoaderCircle,
  RefreshCw,
  Search,
  X,
} from '../icons'
import { modelCatalogs } from '../models'
import {
  effectiveSpeed,
  offersSpeed,
  showsSpeed,
  speedTier,
} from '../speeds'
import { iconButton } from '../ui'
import Icon from './Icon.vue'

// One menu for the coding agent, its model, the reasoning effort and Codex's speed. Empty values
// keep inheriting the agent's (or provider's) defaults. The speed is kept across model changes.
const props = withDefaults(defineProps<{
  inherit?: boolean
  defaultModel?: string
  defaultReasoning?: string
  defaultSpeed?: string
  defaultSource?: 'agent' | 'conversation'
  disabled?: boolean
  switching?: boolean
  variant?: 'pill' | 'field'
}>(), {
  defaultModel: '',
  defaultReasoning: '',
  defaultSpeed: '',
  defaultSource: 'agent',
  variant: 'pill',
})
const provider = defineModel<Provider>('provider', { default: 'codex' })
const model = defineModel<string>('model', { default: '' })
const reasoning = defineModel<string>('reasoning', { default: '' })
const speed = defineModel<string>('speed', { default: '' })

const providers = (Object.keys(codingAgents) as Provider[]).map(value => ({ value, ...codingAgents[value], ...brands[value] }))
const order = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra']
const id = useId()
const trigger = ref<HTMLButtonElement>()
const dialog = ref<HTMLDialogElement>()
const open = ref(false)
const query = ref('')
const position = ref<CSSProperties>({})
const catalog = computed(() => modelCatalogs[provider.value].catalog)
const reload = () => modelCatalogs[provider.value].load()
const current = computed(() => providers.find(item => item.value === provider.value) ?? providers[0])
const effectiveModel = computed(() => model.value || props.defaultModel || catalog.value.models.find(item => item.isDefault)?.model || '')
// A model offered by several providers is one entry. Its own id runs on the first available
// route; a route's id pins that provider.
const routeOf = (item: CodingAgentModel, value: string) => item.routes?.find(route => route.model === value)
const matches = (item: CodingAgentModel, value: string) => item.model === value || !!routeOf(item, value)
const selected = computed(() => catalog.value.models.find(item => matches(item, effectiveModel.value)))
const route = computed(() => selected.value && routeOf(selected.value, effectiveModel.value))
const fallback = computed(() => {
  const value = props.defaultModel || catalog.value.models.find(model => model.isDefault)?.model || ''
  const item = catalog.value.models.find(item => matches(item, value))
  return item?.displayName || item?.model || props.defaultModel
})
const modelLabel = computed(() => [selected.value?.displayName || selected.value?.model || effectiveModel.value || 'Default model', route.value?.providerName].filter(Boolean).join(' · '))
const defaultEffort = computed(() => (props.inherit && !model.value ? props.defaultReasoning || selected.value?.defaultReasoningEffort : selected.value?.defaultReasoningEffort) || '')
const effectiveEffort = computed(() => reasoning.value || defaultEffort.value)
// Speeds Codex names in this catalog, to label a saved speed the selected model lacks.
const knownTiers = computed(() => catalog.value.models.flatMap(item => item.serviceTiers ?? []))
const tiers = computed(() => selected.value?.serviceTiers ?? [])
const inheritedSpeed = computed(() => props.inherit ? props.defaultSpeed : '')
const currentSpeed = computed(() => effectiveSpeed(speed.value, inheritedSpeed.value))
const speedShown = computed(() => showsSpeed(provider.value, selected.value, speed.value || inheritedSpeed.value))
const speedOffered = computed(() => offersSpeed(selected.value, currentSpeed.value))
// A speed the model lacks runs at standard speed, so the trigger never claims it.
const appliedSpeed = computed(() => speedShown.value && speedOffered.value ? currentSpeed.value : STANDARD_SPEED)
const appliedTier = computed(() => speedTier(selected.value, appliedSpeed.value))
const speedDefault = computed(() => inheritedSpeed.value ? props.defaultSource === 'conversation' ? 'Conversation default' : 'Agent default' : 'Default')
const defaultSpeedLabel = computed(() => speedLabel(effectiveSpeed('', inheritedSpeed.value), knownTiers.value))
const unsupportedSpeed = computed(() => !!speed.value && speed.value !== STANDARD_SPEED && !tiers.value.some(tier => tier.id === speed.value))
const summary = computed(() => [current.value.label, modelLabel.value, effectiveEffort.value && `${effortLabel(effectiveEffort.value)} reasoning`, appliedSpeed.value !== STANDARD_SPEED && `${speedLabel(appliedSpeed.value, knownTiers.value)} speed`].filter(Boolean).join(' · '))
// Claude lists its own "default" alias. When the default row already resolves to it,
// show one row instead of two equivalent choices.
const alias = computed(() => !props.inherit || !props.defaultModel ? catalog.value.models.find(item => item.model === 'default') : undefined)
const defaultName = computed(() => provider.value === 'claude' && alias.value?.displayName && !alias.value.displayName.startsWith('Default') ? alias.value.displayName : '')
const visible = computed(() => catalog.value.models.filter(item => (!item.hidden || matches(item, model.value)) && (item !== alias.value || model.value === item.model)))
const models = computed(() => {
  const words = query.value.trim().toLowerCase()
  return words ? visible.value.filter(item => `${item.displayName} ${item.model}`.toLowerCase().includes(words)) : visible.value
})
const missing = computed(() => !!model.value && !catalog.value.models.some(item => matches(item, model.value)))
const inheritedLabel = computed(() => props.defaultSource === 'conversation' ? 'Conversation model' : 'Agent default')
const inheritedDescription = computed(() => props.defaultSource === 'conversation' ? 'keep this conversation’s setting' : 'the agent’s setting')
const defaultOwner = computed(() => props.inherit && !model.value ? props.defaultSource === 'conversation' ? 'Conversation' : 'Agent' : 'Model')
const choices = computed(() => [
  ...(!current.value.requiresModel || (props.inherit && props.defaultModel)
    ? [
        {
          value: '',
          label: props.inherit ? inheritedLabel.value : [`${current.value.label} default`, defaultName.value].filter(Boolean).join(' · '),
          description: (!props.inherit && alias.value?.description) || (fallback.value
            ? `${fallback.value} · ${props.inherit ? inheritedDescription.value : `chosen by ${current.value.label}`}`
            : props.inherit ? 'Follow the agent’s model' : 'Follow provider settings'),
          recommended: !props.inherit && !!alias.value,
          chosen: !model.value,
          routes: props.inherit ? catalog.value.models.find(item => matches(item, props.defaultModel))?.routes?.filter(route => !route.hidden || route.model === props.defaultModel) ?? [] : [],
        },
      ]
    : []),
  ...models.value.map(item => ({
    value: item.model,
    label: item.displayName || item.model,
    description: item.description,
    recommended: item.isDefault,
    chosen: !!model.value && matches(item, model.value),
    routes: item.routes?.filter(route => !route.hidden || route.model === model.value) ?? [],
  })),
  ...(missing.value
    ? [{
        value: model.value,
        label: model.value,
        description: 'Saved model · not in the current catalog',
        recommended: false,
        chosen: true,
        routes: [],
      }]
    : []),
])

watch([provider, () => catalog.value.models, () => props.defaultModel], () => {
  if (current.value.requiresModel && !model.value && !(props.inherit && props.defaultModel)) {
    model.value = catalog.value.models.find(item => item.isDefault && !item.hidden)?.model
      || catalog.value.models.find(item => !item.hidden)?.model || ''
  }
}, { immediate: true })
const efforts = computed(() => [...new Map(((route.value ?? selected.value)?.supportedReasoningEfforts ?? []).map(effort => [effort.reasoningEffort, effort])).values()]
  .sort((a, b) => (order.indexOf(a.reasoningEffort) + 1 || 99) - (order.indexOf(b.reasoningEffort) + 1 || 99)))
const effortDescription = computed(() => efforts.value.find(effort => effort.reasoningEffort === effectiveEffort.value)?.description)
const unsupported = computed(() => !!reasoning.value && !!selected.value && !efforts.value.some(effort => effort.reasoningEffort === reasoning.value))

function chooseProvider(value: Provider) {
  if (value === provider.value)
    return
  provider.value = value
  model.value = ''
  reasoning.value = ''
  speed.value = ''
  query.value = ''
}

function chooseModel(option: { value: string, chosen: boolean }) {
  if (option.chosen)
    return
  model.value = option.value
  reasoning.value = ''
}

// The speed is independent of the model: changing model keeps it, as `chooseRoute` does.

// Switching between routes of one model keeps a reasoning level they share.
function chooseRoute(value: string) {
  const effort = effectiveEffort.value
  model.value = value
  reasoning.value = efforts.value.some(option => option.reasoningEffort === effort) ? effort : ''
}

function place() {
  const anchor = trigger.value?.getBoundingClientRect()
  if (!anchor || matchMedia('(width <= 640px)').matches) {
    position.value = {}
    return
  }

  const width = Math.min(Math.max(anchor.width, 400), innerWidth - 24)
  const left = Math.max(12, Math.min(anchor.left, innerWidth - width - 12))
  const above = anchor.top - 20
  const below = innerHeight - anchor.bottom - 20
  const upwards = above > below
  // Override the modal dialog's centered insets so the panel stays attached to its trigger.
  position.value = {
    left: `${left}px`,
    right: 'auto',
    top: upwards ? 'auto' : `${anchor.bottom + 8}px`,
    bottom: upwards ? `${innerHeight - anchor.top + 8}px` : 'auto',
    width: `${width}px`,
    maxWidth: 'none',
    maxHeight: `${Math.min(640, upwards ? above : below)}px`,
  }
}

async function show() {
  if (props.disabled || open.value)
    return
  open.value = true
  void reload()
  place()
  await nextTick()
  dialog.value?.showModal()
  dialog.value?.querySelector<HTMLElement>('[data-model][aria-checked="true"]')?.focus({ preventScroll: true })
  dialog.value?.querySelector('[data-model][aria-checked="true"]')?.scrollIntoView({ block: 'nearest' })
  addEventListener('resize', place)
}

function close() {
  if (!open.value)
    return
  dialog.value?.close()
  open.value = false
  query.value = ''
  removeEventListener('resize', place)
  trigger.value?.focus({ preventScroll: true })
}

// The trigger names the model and effort in use, so it needs a catalog checked this session.
// Later refreshes wait for the menu to open.
watch(provider, () => {
  if (open.value || (catalog.value.checkedAt === null && !catalog.value.loading && !catalog.value.error))
    void reload()
}, { immediate: true })
onBeforeUnmount(() => removeEventListener('resize', place))
</script>

<template>
  <button
    ref="trigger"
    type="button"
    class="assistant-trigger group flex min-w-0 items-center transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50"
    :class="variant === 'field' ? 'col-span-full w-full gap-3 rounded-xl border border-line bg-raised px-3.5 py-3 text-left enabled:hover:border-control' : 'h-8 max-w-full gap-1.5 rounded-full bg-hover/70 pr-2 pl-1.5 text-2xs enabled:hover:bg-hover phone:h-9'"
    :disabled="disabled"
    aria-haspopup="dialog"
    :aria-expanded="open"
    :aria-label="`Agent, model, reasoning and speed: ${summary}`"
    :title="summary"
    @click="show"
  >
    <span class="grid shrink-0 place-items-center" :class="variant === 'field' ? 'size-10 rounded-xl bg-hover/70' : 'size-5'">
      <Icon :name="current.icon" :size="variant === 'field' ? 20 : 14" :class="current.text" />
    </span>
    <template v-if="variant === 'field'">
      <span class="flex min-w-0 flex-1 flex-col">
        <span class="text-3xs font-semibold tracking-wide text-muted uppercase">{{ current.label }}</span>
        <span class="truncate text-sm font-semibold text-ink">{{ modelLabel }}</span>
        <span v-if="effectiveEffort || appliedSpeed !== STANDARD_SPEED" class="flex items-center gap-1 text-2xs text-muted">
          <template v-if="effectiveEffort">{{ effortLabel(effectiveEffort) }} reasoning</template>
          <template v-if="appliedSpeed !== STANDARD_SPEED"><span v-if="effectiveEffort" aria-hidden="true">·</span><Icon :name="Gauge" :size="11" />{{ speedLabel(appliedSpeed, knownTiers) }}</template>
        </span>
      </span>
    </template>
    <template v-else>
      <span class="min-w-0 truncate font-semibold text-ink">{{ modelLabel }}</span>
      <span v-if="effectiveEffort" class="flex shrink-0 items-center gap-1.5 text-muted phone:hidden"><span class="size-[3px] rounded-full bg-current" />{{ effortLabel(effectiveEffort) }}</span>
      <span v-if="appliedSpeed !== STANDARD_SPEED" class="flex shrink-0 items-center gap-0.5 text-accent" :title="`${speedLabel(appliedSpeed, knownTiers)} speed`"><Icon :name="Gauge" :size="12" /><span class="phone:hidden">{{ speedLabel(appliedSpeed, knownTiers) }}</span></span>
    </template>
    <Icon
      :name="ChevronDown"
      :size="14"
      class="shrink-0 text-muted transition-transform"
      :class="{ 'rotate-180': open }"
    />
  </button>
  <Teleport to="body">
    <dialog
      v-if="open"
      ref="dialog"
      class="assistant-picker m-0 overflow-y-auto overscroll-contain border border-line bg-raised p-0 text-ink"
      :style="position"
      :aria-labelledby="`${id}-title`"
      @cancel.prevent="close"
      @click="(event) => { if (event.target === dialog) close() }"
    >
      <div class="flex flex-col gap-4 p-4 phone:px-5 phone:pb-[calc(20px+env(safe-area-inset-bottom))]">
        <div class="mx-auto -mt-1 hidden h-1 w-9 rounded-full bg-line phone:block" aria-hidden="true" />
        <header class="flex items-start gap-2">
          <div class="min-w-0 flex-1">
            <h2 :id="`${id}-title`" class="font-heading text-base font-bold">
              Agent &amp; model
            </h2>
            <p class="text-2xs text-muted">
              {{ variant === 'pill' ? 'Applies to the next message' : 'Default for this agent’s runs' }}
            </p>
          </div>
          <button
            type="button"
            :class="iconButton"
            aria-label="Refresh models"
            title="Refresh models"
            :disabled="catalog.loading"
            @click="reload"
          >
            <Icon :name="catalog.loading ? LoaderCircle : RefreshCw" :size="15" :class="{ 'animate-spin': catalog.loading }" />
          </button>
          <button
            type="button"
            :class="iconButton"
            aria-label="Close"
            @click="close"
          >
            <Icon :name="X" :size="16" />
          </button>
        </header>

        <section class="flex flex-col gap-2" :aria-labelledby="`${id}-agent`">
          <h3 :id="`${id}-agent`" class="section-label">
            Coding agent
          </h3>
          <div class="grid grid-cols-3 gap-2" role="radiogroup" aria-label="Coding agent">
            <button
              v-for="item in providers"
              :key="item.value"
              type="button"
              role="radio"
              :aria-checked="provider === item.value"
              :aria-label="item.label"
              class="provider-tile flex flex-col items-center gap-2 rounded-xl border px-2 py-2.5 text-center transition-[border-color,background,box-shadow] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
              :class="provider === item.value ? item.chosen : 'border-line hover:border-control hover:bg-hover/40'"
              :disabled="disabled"
              @click="chooseProvider(item.value)"
            >
              <Icon :name="item.icon" :size="22" :class="item.text" />
              <span class="flex min-w-0 flex-col">
                <span class="text-sm leading-tight font-semibold">{{ item.label }}</span>
                <span class="text-3xs text-muted">{{ item.vendor }}</span>
              </span>
            </button>
          </div>
          <p v-if="switching" class="flex items-start gap-2 rounded-lg bg-soft px-3 py-2 text-2xs text-ink" role="status">
            <Icon :name="Info" :size="14" class="mt-px text-accent" />The next message starts a new {{ current.label }} session with this chat’s context and existing files.
          </p>
        </section>

        <section class="flex flex-col gap-2" :aria-labelledby="`${id}-model`">
          <h3 :id="`${id}-model`" class="section-label">
            Model
          </h3>
          <div v-if="provider === 'opencode'" class="flex gap-2.5 rounded-xl border border-line bg-surface px-3 py-2.5 text-2xs">
            <Icon :name="Info" :size="15" class="mt-0.5 shrink-0 text-accent" />
            <div>
              <p class="font-semibold text-ink">
                Models from your connections
              </p>
              <p class="mt-1 text-muted">
                Access depends on your provider’s plan, balance and Privacy settings. A listed model may still be blocked when you use it.
              </p>
            </div>
          </div>
          <label v-if="visible.length > 6" class="flex items-center gap-2 rounded-lg bg-hover/60 px-3 text-muted focus-within:ring-2 focus-within:ring-accent/30">
            <Icon :name="Search" :size="14" /><span class="sr-only">Search models</span>
            <input
              v-model="query"
              type="search"
              placeholder="Search models…"
              class="min-w-0 flex-1 border-0! bg-transparent! px-0! py-2! text-xs! shadow-none! outline-none!"
            >
          </label>
          <p v-if="catalog.error" class="flex items-center justify-between gap-3 text-2xs text-muted" role="status">
            <span>{{ catalog.error }}</span>
            <button
              type="button"
              class="shrink-0 text-accent underline"
              :disabled="catalog.loading"
              @click="reload"
            >
              Retry
            </button>
          </p>
          <div class="flex flex-col gap-1.5" role="radiogroup" aria-label="Model">
            <template v-for="option in choices" :key="option.value">
              <div class="model-card rounded-xl border transition-[border-color,background]" :class="option.chosen ? 'selected border-accent bg-soft' : 'border-transparent bg-hover/35 hover:bg-hover/60'">
                <button
                  type="button"
                  role="radio"
                  data-model
                  :aria-checked="option.chosen"
                  :aria-label="option.label"
                  :aria-describedby="option.description ? `${id}-${option.value || 'default'}-description` : undefined"
                  class="flex w-full items-center gap-3 rounded-xl px-3.5 py-3 text-left focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
                  :disabled="disabled"
                  @click="chooseModel(option)"
                >
                  <span class="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span class="flex flex-wrap items-center gap-2">
                      <span class="text-sm font-semibold">{{ option.label }}</span>
                      <span v-if="option.recommended" class="rounded-full bg-accent/12 px-2 py-px text-3xs font-semibold text-accent">{{ provider === 'opencode' ? 'Provider default' : 'Recommended' }}</span>
                    </span>
                    <span
                      v-if="option.description"
                      :id="`${id}-${option.value || 'default'}-description`"
                      class="text-2xs text-muted"
                      :class="option.chosen ? 'line-clamp-4' : 'line-clamp-2'"
                    >{{ option.description }}</span>
                  </span>
                  <span class="grid size-5 shrink-0 place-items-center rounded-full" :class="option.chosen ? 'bg-accent text-raised' : 'border-[1.5px] border-control'">
                    <Icon v-if="option.chosen" :name="Check" :size="12" />
                  </span>
                </button>
                <div v-if="option.chosen" class="mx-2.5 mb-2.5 flex flex-col gap-2 rounded-lg bg-raised px-3 py-2.5">
                  <template v-if="option.routes.length > 1">
                    <div class="flex items-baseline justify-between gap-2">
                      <span class="text-2xs font-medium text-muted">Connection</span>
                      <strong class="text-sm text-accent">{{ route?.providerName ?? 'Auto' }}</strong>
                    </div>
                    <div class="flex flex-wrap gap-1.5" role="radiogroup" aria-label="Connection">
                      <button
                        type="button"
                        role="radio"
                        :aria-checked="!route"
                        class="effort-chip"
                        :disabled="disabled"
                        @click="chooseRoute(selected?.model || option.value)"
                      >
                        <Icon v-if="!route" :name="Check" :size="12" />Auto
                      </button>
                      <button
                        v-for="item in option.routes"
                        :key="item.model"
                        type="button"
                        role="radio"
                        :aria-checked="route?.model === item.model"
                        class="effort-chip"
                        :disabled="disabled"
                        @click="chooseRoute(item.model)"
                      >
                        {{ item.providerName }}<span class="opacity-70">· {{ billingLabels[item.billing] }}</span>
                      </button>
                    </div>
                    <p class="text-2xs text-muted">
                      {{ route ? `Runs only on ${route.providerName}. When its limit is reached, the run waits.` : `Tries ${option.routes.map(item => item.providerName).join(', then ')}, in your Connections order.` }}
                    </p>
                    <hr class="my-0.5 border-line">
                  </template>
                  <p v-else-if="option.routes.length" class="text-2xs text-muted">
                    Via {{ option.routes[0].providerName }} · {{ billingLabels[option.routes[0].billing] }}
                  </p>
                  <div class="flex items-baseline justify-between gap-2">
                    <span :id="`${id}-reasoning`" class="text-2xs font-medium text-muted">Reasoning</span>
                    <strong class="text-sm text-accent">{{ effortLabel(effectiveEffort) }}</strong>
                  </div>
                  <div class="flex flex-wrap gap-1.5" role="radiogroup" aria-label="Reasoning">
                    <button
                      type="button"
                      role="radio"
                      :aria-checked="!reasoning"
                      class="effort-chip"
                      :disabled="disabled"
                      @click="reasoning = ''"
                    >
                      <Icon v-if="!reasoning" :name="Check" :size="12" />{{ defaultOwner }} default{{ defaultEffort ? ` · ${effortLabel(defaultEffort)}` : '' }}
                    </button>
                    <button
                      v-for="effort in efforts"
                      :key="effort.reasoningEffort"
                      type="button"
                      role="radio"
                      :aria-checked="reasoning === effort.reasoningEffort"
                      class="effort-chip"
                      :disabled="disabled"
                      @click="reasoning = effort.reasoningEffort"
                    >
                      {{ effortLabel(effort.reasoningEffort) }}
                    </button>
                    <button
                      v-if="unsupported"
                      type="button"
                      role="radio"
                      aria-checked="true"
                      class="effort-chip"
                      disabled
                    >
                      {{ effortLabel(reasoning) }}
                    </button>
                  </div>
                  <p v-if="unsupported" class="text-2xs text-danger">
                    This model doesn’t offer that level. Choose a supported level or use the default.
                  </p>
                  <p v-else-if="!selected" class="text-2xs text-muted">
                    Catalog unavailable for this model. The saved setting is kept.
                  </p>
                  <p v-else-if="!efforts.length" class="text-2xs text-muted">
                    This model has no reasoning setting.
                  </p>
                  <p v-else-if="effortDescription" class="text-2xs text-muted">
                    {{ effortDescription }}
                  </p>
                  <template v-if="speedShown">
                    <hr class="my-0.5 border-line">
                    <div class="flex items-baseline justify-between gap-2">
                      <span class="text-2xs font-medium text-muted">Speed</span>
                      <strong class="text-sm text-accent">{{ speedLabel(appliedSpeed, knownTiers) }}</strong>
                    </div>
                    <div class="flex flex-wrap gap-1.5" role="radiogroup" aria-label="Speed">
                      <button
                        type="button"
                        role="radio"
                        :aria-checked="!speed"
                        class="effort-chip"
                        :disabled="disabled"
                        @click="speed = ''"
                      >
                        <Icon v-if="!speed" :name="Check" :size="12" />{{ speedDefault }} · {{ defaultSpeedLabel }}
                      </button>
                      <button
                        type="button"
                        role="radio"
                        :aria-checked="speed === STANDARD_SPEED"
                        class="effort-chip"
                        :disabled="disabled"
                        @click="speed = STANDARD_SPEED"
                      >
                        Standard
                      </button>
                      <button
                        v-for="tier in tiers"
                        :key="tier.id"
                        type="button"
                        role="radio"
                        :aria-checked="speed === tier.id"
                        class="effort-chip"
                        :disabled="disabled"
                        @click="speed = tier.id"
                      >
                        <Icon :name="Gauge" :size="12" />{{ tier.name }}
                      </button>
                      <button
                        v-if="unsupportedSpeed"
                        type="button"
                        role="radio"
                        aria-checked="true"
                        class="effort-chip"
                        disabled
                      >
                        {{ speedLabel(speed, knownTiers) }}
                      </button>
                    </div>
                    <p v-if="!speedOffered" class="text-2xs text-warning" role="status">
                      This model doesn’t offer {{ speedLabel(currentSpeed, knownTiers) }}. Its runs use Standard speed until you choose a model that does.
                    </p>
                    <p v-else-if="appliedTier" class="text-2xs text-muted">
                      {{ appliedTier.description || `${appliedTier.name} speed` }}<template v-if="appliedTier.limited">
                        · Not every Codex account offers it; runs prefer one that does, otherwise they run at Standard.
                      </template> · <a
                        :href="speedUsageUrl"
                        target="_blank"
                        rel="noopener noreferrer"
                        class="text-accent underline"
                      >How speed uses your plan</a>
                    </p>
                    <p v-else class="text-2xs text-muted">
                      Codex’s standard speed and usage.
                    </p>
                  </template>
                  <p v-else-if="provider !== 'codex'" class="text-2xs text-muted">
                    {{ current.label }} has no speed setting.
                  </p>
                </div>
              </div>
            </template>
          </div>
          <p v-if="catalog.loading && !catalog.models.length" class="flex items-center gap-2 py-2 text-2xs text-muted" role="status">
            <Icon :name="LoaderCircle" :size="14" class="animate-spin" />Loading models…
          </p>
          <p v-else-if="query && !models.length" class="py-3 text-center text-xs text-muted">
            No models match “{{ query }}”.
          </p>
          <label v-if="!catalog.loading && !catalog.models.length" class="text-2xs text-muted">Model name<input
            :value="model"
            maxlength="100"
            placeholder="Use the provider default"
            class="mt-1"
            @change="(event) => { const value = (event.target as HTMLInputElement).value.trim(); chooseModel({ value, chosen: value === model }) }"
          ></label>
        </section>
      </div>
    </dialog>
  </Teleport>
</template>

<style scoped>
.assistant-picker {
  position: fixed;
  border-radius: 18px;
  box-shadow: 0 18px 60px light-dark(#12112330, #00000060), 0 2px 8px light-dark(#1211230d, #00000030);
  animation: picker-in 140ms ease-out;
}
.assistant-picker::backdrop { background: transparent; }
.section-label {
  font-size: 10px;
  font-weight: 600;
  letter-spacing: .06em;
  text-transform: uppercase;
  color: var(--color-muted);
}
.effort-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  min-height: 30px;
  padding: 0 11px;
  border: 1px solid var(--color-line);
  border-radius: 999px;
  font-size: 11px;
  font-weight: 500;
  color: var(--color-ink);
  transition: background-color 120ms, border-color 120ms, color 120ms;
}
.effort-chip:hover:not(:disabled) { border-color: var(--color-control); }
.effort-chip[aria-checked="true"] {
  border-color: var(--color-accent);
  background: var(--color-accent);
  color: var(--color-raised);
}
.effort-chip:disabled { cursor: not-allowed; opacity: .6; }
.effort-chip:focus-visible { outline: 2px solid var(--color-accent); outline-offset: 2px; }
@keyframes picker-in {
  from { opacity: 0; transform: translateY(6px) scale(.98); }
}
@media (width <= 640px) {
  .assistant-picker {
    inset: auto 0 0;
    width: 100%;
    max-width: none;
    max-height: calc(100dvh - env(safe-area-inset-top) - 24px);
    border-radius: 22px 22px 0 0;
    border-bottom: 0;
    animation-name: sheet-in;
    animation-duration: 200ms;
  }
  .assistant-picker::backdrop { background: #0a091080; backdrop-filter: blur(3px); }
  .effort-chip { min-height: 36px; }
}
@keyframes sheet-in {
  from { transform: translateY(40px); opacity: .4; }
}
@media (prefers-reduced-motion: reduce) {
  .assistant-picker { animation: none; }
}
</style>
