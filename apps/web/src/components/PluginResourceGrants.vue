<script setup lang="ts">
import type { PluginAccessProfile, PluginOperation, PluginResourceGrant } from '../plugins'
import { computed, onBeforeUnmount, ref } from 'vue'
import { api, state } from '../api'
import { resourceRuleOperations } from '../plugins'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  path: string
  name: string
  operations: PluginOperation[]
  enabled: boolean
}>()
const emit = defineEmits<{ activity: [active: boolean] }>()
const opened = ref(false)
const ready = ref(false)
const busy = ref(false)
const error = ref('')
const rules = ref<PluginResourceGrant[]>([])
const adding = ref(false)
const removing = ref<PluginResourceGrant>()
const agent = ref('')
const profile = ref<PluginAccessProfile | ''>('')
const overrides = ref<Record<string, string>>(Object.create(null))
const limits = ref<Record<string, string>>(Object.create(null))
const profiles = { read: 'Read', contribute: 'Contribution', manage: 'Management' }
const scopeNames = computed(() => [...new Set(props.operations.flatMap(operation => operation.scopes ?? []))])
const draft = computed(() => ({
  agentId: agent.value,
  profile: profile.value || null,
  operations: props.operations.filter(operation => overrides.value[operation.name] === 'allow').map(operation => operation.name),
  deniedOperations: props.operations.filter(operation => overrides.value[operation.name] === 'deny').map(operation => operation.name),
  scopes: Object.fromEntries(scopeNames.value.flatMap((scope) => {
    const values = [...new Set((limits.value[scope] ?? '').split('\n').map(value => value.trim()).filter(Boolean))]
    return values.length ? [[scope, values]] : []
  })),
}))
const allowed = computed(() => resourceRuleOperations(props.operations, draft.value))
const canGrant = computed(() => props.enabled && ready.value && !busy.value
  && state.agents.some(item => item.id === agent.value) && allowed.value.length > 0)

async function load() {
  ready.value = false
  rules.value = (await api<{ items: PluginResourceGrant[] }>(props.path)).items
  ready.value = true
}

async function refresh() {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

function open() {
  opened.value = true
  emit('activity', true)
  void refresh()
}

function close() {
  if (busy.value)
    return
  opened.value = false
  adding.value = false
  removing.value = undefined
  emit('activity', false)
}

function add() {
  agent.value = ''
  profile.value = ''
  overrides.value = Object.create(null)
  limits.value = Object.create(null)
  adding.value = true
}

async function mutate(method: 'POST' | 'DELETE') {
  if (busy.value || !ready.value || !props.enabled || (method === 'POST' && !canGrant.value))
    return
  const target = method === 'DELETE' ? `${props.path}/${removing.value!.id}` : props.path
  const body = method === 'POST' ? JSON.stringify(draft.value) : undefined
  busy.value = true
  error.value = ''
  ready.value = false
  try {
    await api(target, { method, body })
    adding.value = false
    removing.value = undefined
    await load()
  }
  catch (e) {
    // A lost response may already have changed permissions. Never repost automatically.
    error.value = `${(e as Error).message} Refresh the rules before making another change.`
    adding.value = false
    removing.value = undefined
  }
  finally {
    busy.value = false
  }
}

onBeforeUnmount(() => emit('activity', false))
</script>

<template>
  <UiButton size="small" :disabled="!enabled" @click="open">
    Agent access
  </UiButton>
  <Modal v-if="opened" :title="`Agent access · ${name}`" @close="close">
    <div class="grid min-w-0 gap-4 p-6">
      <p class="text-sm text-muted">
        No agent has access by default. Rules add access together. A denial only affects its own rule; another rule may still allow the operation.
        The service’s permissions and personal sign-in requirements still apply.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <UiButton size="small" :disabled="busy || adding || Boolean(removing)" @click="refresh">
        {{ busy ? 'Loading rules…' : 'Refresh rules' }}
      </UiButton>
      <template v-if="ready">
        <p v-if="!rules.length" class="text-sm text-muted">
          No access rules yet.
        </p>
        <article
          v-for="rule in rules"
          :key="rule.id"
          class="grid min-w-0 gap-2 rounded-lg border border-line p-3"
          aria-label="Access rule"
        >
          <b class="break-words text-sm">{{ state.agents.find(item => item.id === rule.agentId)?.name ?? 'Removed agent' }} · {{ rule.profile ? profiles[rule.profile] : 'Selected operations' }}</b>
          <p class="break-words text-xs">
            Allowed by this rule: {{ resourceRuleOperations(operations, rule).map(operation => operation.name).join(', ') || 'None' }}
          </p>
          <p v-if="rule.operations.length" class="break-words text-xs">
            Explicitly allowed: {{ rule.operations.join(', ') }}
          </p>
          <p v-if="rule.deniedOperations.length" class="break-words text-xs">
            Denied in this rule: {{ rule.deniedOperations.join(', ') }}
          </p>
          <p v-for="(values, scope) in rule.scopes" :key="scope" class="break-words text-xs">
            {{ scope }}: {{ values.join(', ') }}
          </p>
          <p v-if="!Object.keys(rule.scopes).length" class="text-xs text-muted">
            No additional scope limits.
          </p>
          <UiButton size="small" :disabled="busy || adding || Boolean(removing) || !enabled" @click="removing = rule">
            Revoke rule
          </UiButton>
          <div v-if="removing?.id === rule.id" class="grid gap-2" role="alert">
            <p class="text-xs">
              Revoke this access rule? Other rules keep their own permissions.
            </p>
            <div class="flex gap-2">
              <UiButton size="small" :disabled="busy" @click="removing = undefined">
                Cancel revoke
              </UiButton>
              <UiButton size="small" :disabled="busy || !enabled" @click="mutate('DELETE')">
                Confirm revoke
              </UiButton>
            </div>
          </div>
        </article>
        <UiButton v-if="!adding" :disabled="busy || Boolean(removing) || !enabled || rules.length >= 64" @click="add">
          Add access rule
        </UiButton>
        <form
          v-else
          class="grid min-w-0 gap-4"
          aria-label="New access rule"
          @submit.prevent="mutate('POST')"
        >
          <label>Agent<select v-model="agent" :disabled="busy || !enabled">
            <option value="">Choose an agent</option>
            <option v-for="item in state.agents" :key="item.id" :value="item.id">{{ item.name }}</option>
          </select></label>
          <label>Profile<select v-model="profile" :disabled="busy || !enabled">
            <option value="">Only selected operations</option>
            <option v-for="(label, value) in profiles" :key="value" :value="value">{{ label }}</option>
          </select></label>
          <p class="text-xs text-muted">
            Contribution includes Read. Management includes both. Choose exceptions below.
          </p>
          <label v-for="operation in operations" :key="operation.name" class="min-w-0 break-words">
            {{ operation.name }}<select v-model="overrides[operation.name]" :disabled="busy || !enabled">
              <option :value="undefined">Use profile</option>
              <option value="allow">Allow explicitly</option>
              <option value="deny">Deny in this rule</option>
            </select>
          </label>
          <p class="text-xs text-muted">
            Optional limits: one allowed value per line. Empty means no additional limit. Operations without every configured scope are excluded.
          </p>
          <label v-for="scope in scopeNames" :key="scope">
            Limit {{ scope }}<textarea v-model="limits[scope]" rows="2" :disabled="busy || !enabled" />
          </label>
          <ul aria-label="Operations allowed by this rule" class="break-words text-xs">
            <li v-for="operation in allowed" :key="operation.name">
              {{ operation.name }}
            </li>
          </ul>
          <p v-if="!allowed.length" class="text-xs text-muted">
            This rule allows no operations.
          </p>
          <div class="flex flex-wrap gap-2">
            <UiButton type="submit" :disabled="!canGrant">
              Grant access
            </UiButton>
            <UiButton :disabled="busy" @click="adding = false">
              Cancel rule
            </UiButton>
          </div>
        </form>
      </template>
    </div>
  </Modal>
</template>
