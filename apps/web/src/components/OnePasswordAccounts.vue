<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api, notify } from '../api'
import {
  BrandOnePassword,
  CheckCircle2,
  Pencil,
  Plus,
  Trash2,
} from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

interface Account {
  id: string
  name: string
  enabled: boolean
  agentIds: string[]
}

const accounts = ref<Account[]>([])
const agents = ref<{ id: string, name: string }[]>([])
const loaded = ref(false)
const busy = ref(false)
const error = ref('')
const open = ref(false)
const removing = ref<Account>()
const editing = ref('')
const name = ref('')
const token = ref('')
const enabled = ref(true)
const agentIds = ref<string[]>([])
const sharedWith = computed(() => new Set(accounts.value.filter(a => a.enabled).flatMap(a => a.agentIds).filter(id => agents.value.some(agent => agent.id === id))).size)

async function action(operation: () => Promise<void>) {
  busy.value = true
  error.value = ''
  try {
    await operation()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function load() {
  [accounts.value, agents.value] = await Promise.all([api('/onepassword'), api('/agents')])
  loaded.value = true
}

function edit(account?: Account) {
  editing.value = account?.id ?? ''
  name.value = account?.name ?? ''
  token.value = ''
  enabled.value = account?.enabled ?? true
  agentIds.value = (account?.agentIds ?? []).filter(id => agents.value.some(agent => agent.id === id))
  error.value = ''
  open.value = true
}

function close() {
  if (busy.value)
    return
  token.value = ''
  open.value = false
}

async function save() {
  await action(async () => {
    await api(`/onepassword${editing.value ? `/${editing.value}` : ''}`, {
      method: editing.value ? 'PUT' : 'POST',
      body: JSON.stringify({
        name: name.value,
        enabled: enabled.value,
        agentIds: agentIds.value,
        ...(token.value.trim() ? { token: token.value.trim() } : {}),
      }),
    })
    token.value = ''
    open.value = false
    notify('1Password account saved')
    await load()
  })
}

async function test(account: Account) {
  await action(async () => {
    await api(`/onepassword/${account.id}/test`, { method: 'POST' })
    notify(`1Password: ${account.name} is connected`)
  })
}

onMounted(() => action(load))
</script>

<template>
  <section aria-label="1Password accounts" class="grid gap-3 border-b border-line px-4.5 py-4 last:border-0 phone:px-4">
    <div class="flex items-center gap-3">
      <span class="grid size-[38px] shrink-0 place-items-center rounded-[30%] bg-hover text-ink"><Icon :name="BrandOnePassword" :size="19" /></span>
      <div class="min-w-0 flex-1">
        <h3 class="text-sm font-semibold">
          1Password
        </h3>
        <p class="truncate text-xs text-muted">
          {{ !loaded ? 'Loading…' : accounts.length ? `${accounts.length} service ${accounts.length === 1 ? 'account' : 'accounts'} · shared with ${sharedWith} ${sharedWith === 1 ? 'agent' : 'agents'}` : 'No 1Password service accounts yet.' }}
        </p>
      </div>
      <UiButton size="small" :disabled="busy || !loaded" @click="edit()">
        <Icon :name="Plus" :size="14" />Add service account
      </UiButton>
    </div>
    <UiAlert v-if="error && !open && !removing">
      {{ error }}
    </UiAlert>
    <UiButton
      v-if="!loaded && error"
      size="small"
      :disabled="busy"
      @click="action(load)"
    >
      Retry loading 1Password
    </UiButton>
    <ul v-if="accounts.length" class="grid gap-1.5">
      <li v-for="account in accounts" :key="account.id" class="flex items-center gap-3 rounded-xl bg-inset py-2 pr-2 pl-3.5">
        <span class="min-w-0 flex-1">
          <span class="block truncate text-sm font-semibold">{{ account.name }}</span>
          <span class="block text-xs text-muted">{{ account.enabled ? 'Enabled' : 'Disabled' }} · {{ account.agentIds.filter(id => agents.some(agent => agent.id === id)).length }} authorized agents</span>
        </span>
        <button
          type="button"
          :class="iconButton"
          :disabled="busy"
          :aria-label="`Test ${account.name}`"
          title="Test connection"
          @click="test(account)"
        >
          <Icon :name="CheckCircle2" :size="16" />
        </button>
        <button
          type="button"
          :class="iconButton"
          :disabled="busy"
          :aria-label="`Edit ${account.name}`"
          title="Edit access and token"
          @click="edit(account)"
        >
          <Icon :name="Pencil" :size="16" />
        </button>
        <button
          type="button"
          :class="iconButton"
          :disabled="busy"
          :aria-label="`Delete ${account.name}`"
          title="Delete"
          @click="removing = account; error = ''"
        >
          <Icon :name="Trash2" :size="16" />
        </button>
      </li>
    </ul>
    <Modal v-if="open" :title="editing ? 'Edit 1Password account' : 'Add 1Password account'" @close="close">
      <form class="p-6 space-y-4" @submit.prevent="save">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <fieldset :disabled="busy" class="space-y-4">
          <label>Name<input
            v-model="name"
            required
            maxlength="100"
            autocomplete="off"
          ></label>
          <label>Service account token<input
            v-model="token"
            type="password"
            :required="!editing"
            maxlength="16384"
            autocomplete="new-password"
            spellcheck="false"
            :placeholder="editing ? 'Leave blank to keep saved token' : 'ops_…'"
          ></label>
          <p class="text-muted text-sm">
            Tokens are encrypted on the server and never shown again. Only read access is exposed to agents; choose the vault permissions in 1Password.
          </p>
          <label class="checkbox flex flex-row items-center gap-2"><input v-model="enabled" type="checkbox">Enable this account</label>
          <h3>Authorized agents</h3>
          <p class="text-muted text-sm">
            Access is off by default, including for Main agent. Uncheck an agent to revoke future reads immediately. Secrets already retrieved cannot be recalled.
          </p>
          <label v-for="agent in agents" :key="agent.id" class="checkbox flex flex-row items-center gap-2"><input v-model="agentIds" type="checkbox" :value="agent.id">{{ agent.name }}</label>
        </fieldset>
        <div class="flex justify-end gap-2">
          <UiButton :disabled="busy" @click="close">
            Cancel
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="busy || !name.trim() || (!editing && !token.trim())">
            Save account
          </UiButton>
        </div>
      </form>
    </Modal>
    <Modal v-if="removing" title="Delete 1Password account" @close="!busy && (removing = undefined)">
      <div class="p-6 space-y-4">
        <p>Delete {{ removing.name }} and its stored token? All agents will lose access.</p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton :disabled="busy" @click="action(async () => { await api(`/onepassword/${removing!.id}`, { method: 'DELETE' }); removing = undefined; await load() })">
          Delete account
        </UiButton>
      </div>
    </Modal>
  </section>
</template>
