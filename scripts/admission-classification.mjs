// account_wait_reason is shared by node, account and recovery writers. Only the
// node writer's canonical prefix plus its structured facts establish capacity.
export function classifyAdmissionWait(run) {
  if (run === null || typeof run !== 'object' || Array.isArray(run) || run.status !== 'queued')
    return null

  const account = run.accountRequired
  if (typeof account === 'string' && account.trim().length > 0)
    return 'account'

  const reason = run.waitReason
  if (typeof reason !== 'string' || reason.length === 0)
    return null

  const nodePrefix = 'Waiting for capacity.'
  const nodeReason = reason === nodePrefix || reason.startsWith(`${nodePrefix} `)
  const providerReason = /\b(?:accounts?|usage|quota|rate[ _-]limits?)\b/i.test(reason)
    || /\b(?:model|provider)\b[\s\S]+\b(?:capacity|busy|overloaded)\b/i.test(reason)
  if (!nodeReason && providerReason)
    return 'account'

  const recovering = run.recoveryPending
  if (recovering === true || recovering === 1)
    return 'recovery'

  if (nodeReason) {
    const accountKnownClear = Object.hasOwn(run, 'accountRequired') && account === null
    const recoveryKnownClear = recovering === false || recovering === 0
    return accountKnownClear && recoveryKnownClear ? 'capacity' : null
  }

  if (['Waiting for the previous execution to stop before recovery.', 'Waiting for the previous VM to stop.', 'Paused for worker restart. This run will resume automatically.', 'Recovering after worker restart.', 'Recovering after a worker error.', 'Resuming saved conversation.'].includes(reason)
    || reason.startsWith('Temporary agent error. Retrying in ')) {
    return 'recovery'
  }

  return null
}
