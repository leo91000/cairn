import type { Agent } from '../shared/contracts'
import { onBeforeUnmount, watch } from 'vue'
import { api, state } from './api'

export function updateAgentPortrait(agent: Agent) {
  const existing = state.agents.find(item => item.id === agent.id)
  if (existing)
    existing.avatar = agent.avatar
}

// One poll for the app, only while a portrait is pending. Reconcile on focus to
// pick up changes made in another tab without polling indefinitely when idle.
export function useAgentPortraits() {
  let timer: ReturnType<typeof setTimeout> | undefined
  let stopped = false
  let loading = false

  async function refreshPortraits() {
    clearTimeout(timer)
    if (loading || stopped || !state.authenticated || document.hidden)
      return
    loading = true
    const before = new Map(state.agents.map(agent => [agent.id, agent.avatar]))
    try {
      const agents = await api<Agent[]>('/agents')
      if (!stopped && state.authenticated) {
        for (const agent of agents) {
          if (state.agents.find(item => item.id === agent.id)?.avatar === before.get(agent.id))
            updateAgentPortrait(agent)
        }
      }
    }
    catch { /* A temporary connection failure must not interrupt the conversation. */ }
    finally {
      loading = false
      schedule()
    }
  }

  function schedule() {
    clearTimeout(timer)
    if (!stopped && state.authenticated && !document.hidden && state.agents.some(agent => agent.avatar?.status === 'generating'))
      timer = setTimeout(refreshPortraits, 2000)
  }

  watch(() => [state.authenticated, ...state.agents.map(agent => agent.avatar?.status)], schedule)
  document.addEventListener('visibilitychange', refreshPortraits)
  window.addEventListener('focus', refreshPortraits)
  onBeforeUnmount(() => {
    stopped = true
    clearTimeout(timer)
    document.removeEventListener('visibilitychange', refreshPortraits)
    window.removeEventListener('focus', refreshPortraits)
  })
}
