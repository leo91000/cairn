import type { Ref } from 'vue'
import type { Member } from './agent-use'
import { computed, ref } from 'vue'
import { grantAgentAccount } from '../../../packages/contracts/generated/grantAgentAccount'
import { listAgentAccounts } from '../../../packages/contracts/generated/listAgentAccounts'
import { revokeAgentAccount } from '../../../packages/contracts/generated/revokeAgentAccount'
import { accessRows, withAccess } from './agent-use'
import { api, state } from './api'

// Members come from the Beacon, which owns the installation's sharing; the installation
// keeps which of them may use each agent. Only the owner reads and changes both.
async function members(): Promise<Member[]> {
  const response = await fetch(`/api/installations/${encodeURIComponent(state.installationId)}/sharing`, {
    headers: { 'X-CSRF-Token': state.csrf },
  })
  const value = await response.json().catch(() => ({}))
  if (!response.ok)
    throw new Error(value?.error || 'Members could not be loaded.')
  return value.members ?? []
}

/** Who may use one agent, changed one member at a time with immediate effect. */
export function useAgentPeople(agentId: Ref<string | null>) {
  const people = ref<Member[]>([])
  const allowed = ref<string[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref('')
  const saving = ref<string[]>([])
  const failure = ref<{ accountId: string, message: string } | null>(null)
  let generation = 0
  let shown: string | null = null

  async function load() {
    const agent = agentId.value
    const current = ++generation
    // Another agent's rows never show while this one loads.
    if (agent !== shown) {
      people.value = []
      allowed.value = []
      loaded.value = false
      failure.value = null
      shown = agent
    }

    if (!agent)
      return
    loading.value = true
    error.value = ''
    try {
      const [listed, accounts] = await Promise.all([members(), listAgentAccounts(api, { agentId: agent })])
      if (current !== generation)
        return
      people.value = listed
      allowed.value = accounts.accounts.map(account => account.accountId)
      loaded.value = true
    }
    catch (cause) {
      if (current === generation)
        error.value = cause instanceof Error ? cause.message : 'Members could not be loaded.'
    }
    finally {
      if (current === generation)
        loading.value = false
    }
  }

  async function setAccess(accountId: string, allow: boolean) {
    const agent = agentId.value
    if (!agent || saving.value.includes(accountId))
      return
    saving.value = [...saving.value, accountId]
    failure.value = null
    try {
      const input = { agentId: agent, accountId }
      const output = allow ? await grantAgentAccount(api, input) : await revokeAgentAccount(api, input)
      if (agentId.value === agent)
        allowed.value = withAccess(allowed.value, accountId, output.accounts.map(account => account.accountId))
    }
    catch (cause) {
      failure.value = { accountId, message: cause instanceof Error ? cause.message : 'Access could not be changed.' }
    }
    finally {
      saving.value = saving.value.filter(account => account !== accountId)
    }
  }

  const rows = computed(() => accessRows(people.value, allowed.value))
  // Former members' leftover entries end at the next membership check.
  const count = computed(() => rows.value.filter(row => row.known && row.allowed).length)
  return {
    rows,
    count,
    loading,
    loaded,
    error,
    saving,
    failure,
    load,
    setAccess,
  }
}
