// Shared wait text is not a typed gate. Prefer the facts committed by the
// writers; the few text exceptions must match complete writer-generated forms.
export function classifyAdmissionWait(run) {
  if (run === null || typeof run !== 'object' || Array.isArray(run) || run.status !== 'queued')
    return null

  if (!Object.hasOwn(run, 'accountRequired'))
    return null
  const account = run.accountRequired
  if (account !== null)
    return ['codex', 'claude', 'opencode'].includes(account) ? 'account' : null

  if (!Object.hasOwn(run, 'recoveryPending'))
    return null
  const recovering = run.recoveryPending
  if (recovering === true || recovering === 1)
    return 'recovery'
  if (recovering !== false && recovering !== 0)
    return null

  const reason = run.waitReason
  if (typeof reason !== 'string' || reason.length === 0)
    return null

  const nodePrefix = 'Waiting for capacity.'
  if (reason === nodePrefix || reason.startsWith(`${nodePrefix} `))
    return 'capacity'

  // Busy/usage waits leave accountRequired null. Validate accounts::waiting's
  // entire format against its snapshot inputs, never generic provider keywords.
  if (!Object.hasOwn(run, 'agentProvider') || !['codex', 'claude', 'opencode'].includes(run.agentProvider)
    || !Object.hasOwn(run, 'model') || typeof run.model !== 'string') {
    return null
  }

  const provider = run.agentProvider
  const label = provider === 'claude' ? 'Claude Code' : provider === 'opencode' ? 'OpenCode' : 'Codex'
  if (reason === `Waiting for a free ${label} account slot. The run starts when another finishes.`
    || reason === `Waiting for a ${label} account with available usage.`) {
    return 'account'
  }

  if (provider === 'opencode' && run.model.length > 0
    && reason === `No enabled ${label} account supports model ${run.model}. Choose a model available through your connections or connect its model provider.`) {
    return 'account'
  }

  return null
}
