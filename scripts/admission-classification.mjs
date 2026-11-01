// Legacy wait text shares one column between admission writers and raw provider
// errors. It cannot prove which writer stored it, so use only typed row facts.
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
  return null
}
