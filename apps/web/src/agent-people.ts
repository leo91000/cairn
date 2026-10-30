import type { Ref } from 'vue'
import type { Member } from './agent-use'
import { computed, ref } from 'vue'
import { grantAgentAccount } from '../../../packages/contracts/generated/grantAgentAccount'
import { listAgentAccounts } from '../../../packages/contracts/generated/listAgentAccounts'
import { revokeAgentAccount } from '../../../packages/contracts/generated/revokeAgentAccount'
import { accessRows, withAccess } from './agent-use'
import { api, ApiError, state } from './api'

// Members come from the Beacon, which owns the installation's sharing; the installation
// keeps which of them may use each agent. Only the owner reads and changes both.
async function sharing(): Promise<{ members: Member[], invitations: Member[] }> {
  const response = await fetch(`/api/installations/${encodeURIComponent(state.installationId)}/sharing`, {
    headers: { 'X-CSRF-Token': state.csrf },
  })
  const value = await response.json().catch(() => ({}))
  if (!response.ok)
    throw new Error(value?.error || 'Members could not be loaded.')
  return { members: value.members ?? [], invitations: value.invitations ?? [] }
}

/** Who may use one agent, changed one member at a time with immediate effect. */
export function useAgentPeople(agentId: Ref<string | null>) {
  const people = ref<Member[]>([])
  // Invited people get a switch once they join.
  const invited = ref<Member[]>([])
  const allowed = ref<string[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref('')
  // An installation older than account access has no such list: every member uses every agent.
  const outdated = ref(false)
  // Changes in flight, per agent: the page is reused across agents, and one agent's pending
  // change or refusal never shows on another's rows.
  const pending = ref<Array<{ agent: string, accountId: string }>>([])
  const saving = computed(() => pending.value.filter(entry => entry.agent === agentId.value).map(entry => entry.accountId))
  const failure = ref<{ accountId: string, message: string } | null>(null)
  let generation = 0
  let shown: string | null = null

  async function load() {
    const agent = agentId.value
    const current = ++generation
    // Another agent's rows never show while this one loads.
    if (agent !== shown) {
      people.value = []
      invited.value = []
      allowed.value = []
      loaded.value = false
      outdated.value = false
      failure.value = null
      shown = agent
    }

    if (!agent)
      return
    loading.value = true
    error.value = ''
    try {
      const [listed, accounts] = await Promise.all([sharing(), listAgentAccounts(api, { agentId: agent })])
      if (current !== generation)
        return
      people.value = listed.members
      invited.value = listed.invitations
      allowed.value = accounts.accounts.map(account => account.accountId)
      outdated.value = false
      loaded.value = true
    }
    catch (cause) {
      if (current !== generation)
        return
      // Only the route itself missing: a deleted agent answers « Agent not found ».
      outdated.value = cause instanceof ApiError && cause.status === 404 && cause.message === 'Not found'
      if (!outdated.value)
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
    pending.value = [...pending.value, { agent, accountId }]
    failure.value = null
    try {
      const input = { agentId: agent, accountId }
      const output = allow ? await grantAgentAccount(api, input) : await revokeAgentAccount(api, input)
      if (agentId.value === agent)
        allowed.value = withAccess(allowed.value, accountId, output.accounts.map(account => account.accountId))
    }
    catch (cause) {
      if (agentId.value === agent)
        failure.value = { accountId, message: cause instanceof Error ? cause.message : 'Access could not be changed.' }
    }
    finally {
      pending.value = pending.value.filter(item => item.agent !== agent || item.accountId !== accountId)
    }
  }

  const rows = computed(() => accessRows(people.value, allowed.value))
  // Former members' leftover entries end at the next membership check.
  const count = computed(() => rows.value.filter(row => row.known && row.allowed).length)
  return {
    rows,
    invited,
    count,
    loading,
    loaded,
    error,
    outdated,
    saving,
    failure,
    load,
    setAccess,
  }
}
