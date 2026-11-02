const workspaceTimeout = /workspace.*timed out/i
const storageFailure = /space|disk|sqlite|I\/O|ublk/i
const timeoutFailure = /timeout|timed out/i
const authorizationFailure = /\b(?:unauthori[sz]ed|forbidden|authentication|401|403)\b/i
const accountLimit = /\b(?:insufficient_quota|(?:quota|usage[ _-]limit|rate[ _-]limit)[ _-](?:exceeded|exhausted|reached)|(?:reached|exceeded|exhausted)(?: your| the)? (?:quota|usage limit|rate limit))\b/i
const providerCapacity = /\b(?:selected\s+)?model\s+is\s+at\s+capacity\b/i
const providerOverloadedCode = /\bserverOverloaded\b/i

function hasStructuredProviderOverload(error) {
  return error !== null && typeof error === 'object'
    && Object.hasOwn(error, 'codexErrorInfo')
    && error.codexErrorInfo === 'serverOverloaded'
}

// Return only a bounded category; callers must not persist or emit the source error.
export function classifyFailure(error) {
  const message = typeof error === 'string' ? error : ''

  if (workspaceTimeout.test(message))
    return 'workspace_timeout'
  if (storageFailure.test(message))
    return 'storage'
  if (timeoutFailure.test(message))
    return 'timeout'
  if (authorizationFailure.test(message))
    return 'other'
  if (accountLimit.test(message))
    return 'account'
  if (hasStructuredProviderOverload(error) || providerCapacity.test(message) || providerOverloadedCode.test(message))
    return 'provider_capacity'

  return 'other'
}
