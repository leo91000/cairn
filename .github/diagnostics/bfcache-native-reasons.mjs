import { closedTargetBound } from './closed-target-error.mjs'

// Both functions run in the existing page's main world. Private identity hashes
// stay in sessionStorage, already used by the original availability journal.
export async function armNativeHistoryIdentity() {
  const receipt = { armed: false, mainFrame: false, nativeHistoryPresent: false, secureContext: false }
  try {
    receipt.mainFrame = window.top === window && document.defaultView === window
    receipt.secureContext = globalThis.isSecureContext === true
    const key = window.navigation?.currentEntry?.key
    const doc = window.cairnAvailabilityDocument
    receipt.nativeHistoryPresent = typeof key === 'string' && key.length > 0
    if (!receipt.mainFrame || !receipt.nativeHistoryPresent || typeof doc !== 'string' || !doc || !crypto.subtle)
      return receipt
    const digest = async value => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))), byte => byte.toString(16).padStart(2, '0')).join('')
    const [entry, href, origin, original] = await Promise.all([key, location.href, location.origin, doc].map(digest))
    sessionStorage.setItem('__cairn_diag_bfcache_nrr_v1', JSON.stringify({ schema: 1, entry, href, origin, original }))
    receipt.armed = true
  }
  catch {}
  return receipt
}

export async function readNativeHistoryReasons() {
  const out = {
    apiPresent: false, nativeEntry: false, mainFrame: false, identityPresent: false,
    historyEntryMatched: false, locationMatched: false, originalDocumentSame: false,
    stableContext: false, navigationType: null, reasonsPresent: false, masked: false,
    truncated: false, malformed: false, evaluationFailed: false,
    nodeCount: 0, reasonCount: 0, tree: null, projectionComplete: false,
  }
  try {
    out.mainFrame = window.top === window && document.defaultView === window
    const markerText = sessionStorage.getItem('__cairn_diag_bfcache_nrr_v1')
    sessionStorage.removeItem('__cairn_diag_bfcache_nrr_v1')
    const marker = markerText ? JSON.parse(markerText) : null
    out.identityPresent = marker?.schema === 1 && ['entry', 'href', 'origin', 'original'].every(key => typeof marker[key] === 'string' && /^[a-f0-9]{64}$/.test(marker[key]))
    const ctor = globalThis.PerformanceNavigationTiming
    const getter = ctor && Object.getOwnPropertyDescriptor(ctor.prototype, 'notRestoredReasons')?.get
    out.apiPresent = typeof getter === 'function'
    const entries = Performance.prototype.getEntriesByType.call(performance, 'navigation')
    const entry = entries.length === 1 ? entries[0] : null
    out.nativeEntry = Boolean(ctor && entry instanceof ctor && Object.getPrototypeOf(entry) === ctor.prototype)
    if (!out.mainFrame || !out.identityPresent || !out.apiPresent || !out.nativeEntry)
      return out
    const key = window.navigation?.currentEntry?.key
    const href = location.href
    const doc = window.cairnAvailabilityDocument
    if (typeof key !== 'string' || typeof doc !== 'string')
      return out
    const digest = async value => Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))), byte => byte.toString(16).padStart(2, '0')).join('')
    const [entryHash, hrefHash, originHash, docHash, timingHash] = await Promise.all([key, href, location.origin, doc, entry.name].map(digest))
    out.historyEntryMatched = entryHash === marker.entry
    out.locationMatched = hrefHash === marker.href && originHash === marker.origin && timingHash === marker.href
    out.originalDocumentSame = docHash === marker.original
    out.stableContext = key === window.navigation?.currentEntry?.key && href === location.href && doc === window.cairnAvailabilityDocument
    out.navigationType = ['navigate', 'reload', 'back_forward', 'prerender'].includes(entry.type) ? entry.type : null
    if (!out.historyEntryMatched || !out.locationMatched || !out.stableContext || out.navigationType !== 'back_forward')
      return out
    // Use the prototype accessor on the real navigation entry, not an own field.
    const native = Reflect.apply(getter, entry, [])
    out.reasonsPresent = native !== null && native !== undefined
    if (!out.reasonsPresent)
      return out
    const seen = new Set()
    const walk = (node, depth) => {
      if (!node || typeof node !== 'object' || seen.has(node)) {
        out.malformed = true
        return null
      }
      if (depth > 16 || out.nodeCount >= 128) {
        out.truncated = true
        return null
      }
      seen.add(node)
      out.nodeCount++
      const reasons = node.reasons
      const children = node.children
      const projected = { reasonCodes: [], children: [] }
      if (reasons === null) {
        out.masked = true
      }
      else if (!Array.isArray(reasons)) {
        out.malformed = true
      }
      else {
        for (const item of reasons) {
          const code = item?.reason
          // Format validation protects output; it does not classify cause wording.
          if (typeof code !== 'string' || !/^[\p{L}\p{N}][\p{L}\p{N} ._():-]{0,255}$/u.test(code)) {
            out.malformed = true
            continue
          }
          if (out.reasonCount >= 512) {
            out.truncated = true
            break
          }
          projected.reasonCodes.push(code)
          out.reasonCount++
          if (code.toLowerCase() === 'masked')
            out.masked = true
        }
      }
      if (!Array.isArray(children)) {
        out.malformed = true
      }
      else {
        for (const child of children) {
          if (out.truncated)
            break
          projected.children.push(walk(child, depth + 1))
        }
      }
      return projected
    }
    out.tree = walk(native, 0)
    out.projectionComplete = Boolean(out.tree && out.reasonCount > 0 && !out.masked && !out.truncated && !out.malformed)
  }
  catch {
    out.evaluationFailed = true
    out.projectionComplete = false
    out.tree = null
    out.reasonCount = 0
    out.nodeCount = 0
  }
  return out
}

const flags = ['apiPresent', 'nativeEntry', 'mainFrame', 'identityPresent', 'historyEntryMatched', 'locationMatched', 'originalDocumentSame', 'stableContext', 'reasonsPresent', 'masked', 'truncated', 'malformed', 'evaluationFailed', 'projectionComplete']
const keys = new Set([...flags, 'navigationType', 'nodeCount', 'reasonCount', 'tree'])

export function validateClosedProjection(value) {
  if (!value || typeof value !== 'object' || Object.keys(value).length !== keys.size || Object.keys(value).some(key => !keys.has(key)))
    return false
  if (flags.some(key => typeof value[key] !== 'boolean') || ![null, 'navigate', 'reload', 'back_forward', 'prerender'].includes(value.navigationType))
    return false
  if (![value.nodeCount, value.reasonCount].every(n => Number.isInteger(n) && n >= 0))
    return false
  let nodes = 0
  let reasons = 0
  const visit = (node, depth) => {
    if (node === null)
      return value.projectionComplete === false
    if (!node || depth > 16 || ++nodes > 128 || Object.keys(node).sort().join(',') !== 'children,reasonCodes' || !Array.isArray(node.reasonCodes) || !Array.isArray(node.children))
      return false
    for (const code of node.reasonCodes) {
      if (typeof code !== 'string' || !/^[\p{L}\p{N}][\p{L}\p{N} ._():-]{0,255}$/u.test(code))
        return false
      if (code.toLowerCase() === 'masked' && value.masked !== true)
        return false
      reasons++
    }
    return node.children.every(child => visit(child, depth + 1))
  }
  if (value.tree === null)
    return value.nodeCount === 0 && value.reasonCount === 0 && value.projectionComplete === false
  if (value.projectionComplete && (!value.reasonsPresent || value.reasonCount === 0 || value.masked || value.truncated || value.malformed || value.evaluationFailed))
    return false
  return visit(value.tree, 0) && nodes === value.nodeCount && reasons === value.reasonCount
}

export function nativeProjectionUsable(projected) {
  return Boolean(validateClosedProjection(projected) && projected.apiPresent && projected.nativeEntry && projected.mainFrame && projected.identityPresent
    && projected.historyEntryMatched && projected.locationMatched && projected.stableContext && projected.reasonsPresent
    && projected.navigationType === 'back_forward' && !projected.originalDocumentSame && projected.projectionComplete
    && !projected.masked && !projected.truncated && !projected.malformed && !projected.evaluationFailed)
}

export async function acquireNativeReasonDiagnostic(control) {
  const failures = []
  const receipt = {
    diagnosticOnly: true, productQualified: false, checkpointObserved: false,
    checkpointPassed: false, checkpointFailed: false, checkpointConsistent: false,
    historyAttempts: 0, historyCompleted: false, projectionReceived: false,
    nativeReadUsable: false, browserVersion: null, native: null,
  }
  try {
    await control.checkpoint()
    receipt.checkpointPassed = true
  }
  catch (cause) {
    receipt.checkpointFailed = true
    failures.push(cause)
  }
  try {
    const values = control.checkpointFlags()
    receipt.checkpointObserved = values.length === 1 && values[0].trusted === true && typeof values[0].persisted === 'boolean'
    receipt.checkpointConsistent = receipt.checkpointObserved && receipt.checkpointPassed === values[0].persisted
    if (!receipt.checkpointConsistent)
      throw new Error('BF checkpoint provenance or strict assertion is unknown')
    const version = control.browserVersion()
    if (typeof version !== 'string' || !/^[A-Za-z0-9.+_() -]{1,96}$/.test(version))
      throw new Error('Browser version is unavailable')
    receipt.browserVersion = version
    receipt.historyAttempts++
    await closedTargetBound(control.history, 'first history navigation')
    receipt.historyCompleted = true
    await closedTargetBound(control.harvest, 'post-history original journal')
    const projected = await closedTargetBound(control.observe, 'native reasons read')
    if (!validateClosedProjection(projected))
      throw new Error('Native reason projection is not closed')
    receipt.projectionReceived = true
    receipt.native = projected
    receipt.nativeReadUsable = nativeProjectionUsable(projected)
    if (!receipt.nativeReadUsable)
      throw new Error('Native reason acquisition is inconclusive')
  }
  catch (cause) {
    failures.push(cause)
  }
  return { receipt, failures }
}

export function finalizeNativeReasonDiagnostic(receipt, closure) {
  const booleans = ['diagnosticOnly', 'productQualified', 'checkpointObserved', 'checkpointPassed', 'checkpointFailed', 'checkpointConsistent', 'historyCompleted', 'projectionReceived', 'nativeReadUsable']
  const allowed = new Set([...booleans, 'historyAttempts', 'browserVersion', 'native'])
  const closed = Boolean(receipt && typeof receipt === 'object' && Object.keys(receipt).length === allowed.size
    && Object.keys(receipt).every(key => allowed.has(key)) && booleans.every(key => typeof receipt[key] === 'boolean')
    && Number.isInteger(receipt.historyAttempts) && receipt.historyAttempts >= 0 && receipt.historyAttempts <= 1
    && (receipt.browserVersion === null || (typeof receipt.browserVersion === 'string' && /^[A-Za-z0-9.+_() -]{1,96}$/.test(receipt.browserVersion)))
    && (receipt.native === null || validateClosedProjection(receipt.native))
    && receipt.nativeReadUsable === nativeProjectionUsable(receipt.native)
    && (!receipt.nativeReadUsable || (receipt.historyAttempts === 1 && receipt.historyCompleted && receipt.projectionReceived && receipt.checkpointObserved && receipt.checkpointConsistent && receipt.browserVersion !== null)))
  const countsKnown = [closure.cleanupFailureCount, closure.auditFailureCount].every(n => Number.isInteger(n) && n >= 0)
  const out = Object.fromEntries(booleans.map(key => [key, closed && receipt[key] === true]))
  return {
    ...out,
    diagnosticOnly: true,
    productQualified: false,
    receiptClosed: closed,
    historyAttempts: closed ? receipt.historyAttempts : 0,
    browserVersion: closed ? receipt.browserVersion : null,
    native: closed ? receipt.native : null,
    cleanupFailureCount: countsKnown ? closure.cleanupFailureCount : 0,
    auditFailureCount: countsKnown ? closure.auditFailureCount : 0,
    closureCountsKnown: countsKnown,
    identityArmed: closure.arm?.armed === true,
    identityMainFrame: closure.arm?.mainFrame === true,
    identityHistoryPresent: closure.arm?.nativeHistoryPresent === true,
    identitySecureContext: closure.arm?.secureContext === true,
    pageClosed: closure.pageClosed === true,
    httpClosed: closure.httpClosed === true,
    journalAcknowledged: closure.journalAcknowledged === true,
    observationComplete: Boolean(closed && receipt.nativeReadUsable && closure.arm?.armed === true && countsKnown && closure.pageClosed && closure.httpClosed && closure.journalAcknowledged
      && closure.cleanupFailureCount === 0 && closure.auditFailureCount === 0),
  }
}
