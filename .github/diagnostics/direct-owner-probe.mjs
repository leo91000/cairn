import { performance } from 'node:perf_hooks'
import { closedTargetBound } from './closed-target-error.mjs'

// Serialized into the real page. All identities below stay private; publication
// is exclusively through projectDirectOwners. No original promise is replaced.
export function observeDirectOwners() {
  const key = '__c40_direct_owner_journal_v1'
  const api = '__c40DirectOwnerReceipt'
  if (Object.hasOwn(window, api))
    return
  const ownerWindow = window
  const ownerDocument = document
  const fetch = window.fetch
  const json = window.Response.prototype.json
  const any = window.AbortSignal.any
  const abort = window.AbortController.prototype.abort
  const signalGetter = Object.getOwnPropertyDescriptor(window.AbortController.prototype, 'signal').get
  const NativeSource = window.EventSource
  const close = NativeSource.prototype.close
  const then = window.Promise.prototype.then
  const abortedGetter = Object.getOwnPropertyDescriptor(window.AbortSignal.prototype, 'aborted').get
  const reasonGetter = Object.getOwnPropertyDescriptor(window.AbortSignal.prototype, 'reason').get
  const parentSets = new WeakMap()
  const signalOwners = new WeakMap()
  const responses = new WeakMap()
  const sources = new WeakMap()
  const grants = new Map()
  const causes = new WeakMap()
  const abortCalls = []
  let ownerOrdinal = 0
  let sourceOrdinal = 0
  let requestOrdinal = 0
  let observerFailures = 0
  const ownContext = () => ownerWindow.document === ownerDocument && ownerDocument.defaultView === ownerWindow && ownerWindow.top === ownerWindow
  const record = (kind, detail = {}) => {
    try {
      const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
      if (!Array.isArray(journal))
        throw new Error('Unknown private journal')
      const event = {
        kind,
        sequence: journal.length + 1,
        document: typeof window.cairnAvailabilityDocument === 'string' ? window.cairnAvailabilityDocument : null,
        agentsDocument: /\/installations\/[^/]+\/agents$/.test(location.pathname),
        ownContext: ownContext(),
        browserTime: performance.now(),
        ...detail,
      }
      journal.push(event)
      sessionStorage.setItem(key, JSON.stringify(journal))
      const pending = window.__c40RecordDirectOwner(event)
      void Reflect.apply(then, pending, [undefined, () => { observerFailures++ }])
    }
    catch {
      observerFailures++
    }
  }
  const inspect = (value) => {
    if (typeof value !== 'string')
      return null
    const url = new URL(value, location.href)
    if (url.origin !== location.origin)
      return null
    const match = /^\/api\/installations\/[^/]+\/direct\/(authorize|([^/]+)\/(signal|events))$/.exec(url.pathname)
    if (!match)
      return null
    return { category: match[1] === 'authorize' ? 'authorize' : match[3], connection: match[2] ? decodeURIComponent(match[2]) : null }
  }
  const observe = (promise, fulfilled, rejected) => {
    try {
      const observation = Reflect.apply(then, promise, [
        value => { try { fulfilled?.(value) } catch { observerFailures++ } },
        cause => { try { rejected?.(cause) } catch { observerFailures++ } },
      ])
      void Reflect.apply(then, observation, [undefined, () => { observerFailures++ }])
    }
    catch { observerFailures++ }
  }
  const signalState = (signal, cause) => {
    try {
      if (!(signal instanceof window.AbortSignal))
        return { known: false, aborted: null, matches: null }
      const aborted = Reflect.apply(abortedGetter, signal, [])
      return { known: true, aborted, matches: aborted && cause === Reflect.apply(reasonGetter, signal, []) }
    }
    catch { return { known: false, aborted: null, matches: null } }
  }
  const state = (signal, control, generation, cause) => {
    const request = signalState(signal, cause)
    const owner = signalState(control, cause)
    const document = signalState(generation, cause)
    return {
      requestSignalKnown: request.known, requestAborted: request.aborted,
      controlSignalKnown: owner.known, controlAborted: owner.aborted,
      generationSignalKnown: document.known, generationAborted: document.aborted,
      requestReasonMatches: request.matches, controlReasonMatches: owner.matches,
      generationReasonMatches: document.matches,
    }
  }
  const sourceRecord = (kind, meta, source, detail = {}) => {
    try { record(kind, { ...meta, ...detail, readyState: source.readyState }) }
    catch { observerFailures++ }
  }
  const trackParent = (signal, role) => {
    let owner = signalOwners.get(signal)
    if (!owner) {
      owner = { owner: ++ownerOrdinal, role }
      signalOwners.set(signal, owner)
      signal.addEventListener('abort', (event) => {
        const call = abortCalls.findLast(call => call.signal === signal)
        record('parent-abort-event', {
          ...owner,
          nativeTrusted: event.isTrusted === true && event instanceof ownerWindow.Event && event.target === signal && event.currentTarget === signal,
          insideOwnAbortCall: Boolean(call),
          nativeEventTime: event.timeStamp,
        })
      }, { once: true })
    }
    return owner
  }
  window.AbortSignal.any = function (...args) {
    const result = Reflect.apply(any, this, args)
    try {
      // The pinned documentSignal uses exactly [lifetime, caller]. Inspect only
      // ordinary data elements AFTER the original call; never consume an iterator.
      const values = args[0]
      if (!Array.isArray(values))
        return result
      const first = Object.getOwnPropertyDescriptor(values, '0')
      const second = Object.getOwnPropertyDescriptor(values, '1')
      if (values.length === 2 && first && second && 'value' in first && 'value' in second
        && first.value instanceof window.AbortSignal && second.value instanceof window.AbortSignal && first.value !== second.value) {
        parentSets.set(result, { generation: first.value, control: second.value })
      }
    }
    catch { observerFailures++ }
    return result
  }
  window.AbortController.prototype.abort = function (...args) {
    let signal
    try { signal = Reflect.apply(signalGetter, this, []) } catch {}
    const owner = signal && signalOwners.get(signal)
    if (!owner)
      return Reflect.apply(abort, this, args)
    const call = { signal }
    abortCalls.push(call)
    record('parent-abort-call-enter', { ...owner, alreadyAborted: signal.aborted })
    try {
      return Reflect.apply(abort, this, args)
    }
    finally {
      record('parent-abort-call-return', { ...owner, aborted: signal.aborted })
      abortCalls.pop()
    }
  }
  window.fetch = function (...args) {
    let meta
    try {
      const path = inspect(args[0])
      const descriptor = args[1] && Object.getOwnPropertyDescriptor(args[1], 'signal')
      const signal = descriptor && 'value' in descriptor ? descriptor.value : undefined
      const parents = signal && parentSets.get(signal)
      if (path && path.category !== 'events') {
        const control = parents?.control
        const generation = parents?.generation
        meta = { ...path, request: ++requestOrdinal, signal, control, generation, owner: parents ? trackParent(control, 'control').owner : null }
        meta.generationOwner = parents ? trackParent(generation, 'generation').owner : null
        if (signal instanceof window.AbortSignal) {
          signal.addEventListener('abort', (event) => record('request-abort-event', {
            category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner,
            nativeTrusted: event.isTrusted === true && event instanceof ownerWindow.Event && event.target === signal && event.currentTarget === signal,
            insideOwnAbortCall: Boolean(parents && abortCalls.some(call => call.signal === control || call.signal === generation)),
            nativeEventTime: event.timeStamp,
          }), { once: true })
        }
        record('control-start', { category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner, parentIdentityKnown: Boolean(parents), ...state(signal, control, generation) })
      }
    }
    catch { observerFailures++ }
    let original
    try { original = Reflect.apply(fetch, this, args) }
    catch (cause) {
      if (meta)
        record('control-threw', { category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner, ...state(meta.signal, meta.control, meta.generation, cause) })
      throw cause
    }
    if (meta) {
      observe(original, (response) => {
        responses.set(response, meta)
        record('control-headers', { category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner, status: response.status })
      }, (cause) => {
        if (cause && (typeof cause === 'object' || typeof cause === 'function'))
          causes.set(cause, [...(causes.get(cause) ?? []), meta])
        record('control-rejected-reaction', {
          category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner,
          causeClass: (cause instanceof window.Error || cause instanceof window.DOMException) && ['TypeError', 'AbortError'].includes(cause.name) ? cause.name : 'other',
          ...state(meta.signal, meta.control, meta.generation, cause),
        })
      })
    }
    return original
  }
  window.Response.prototype.json = function (...args) {
    const original = Reflect.apply(json, this, args)
    const meta = responses.get(this)
    if (meta?.category === 'authorize') {
      observe(original, (value) => {
        const connection = value?.available === true && value?.grant?.claims?.connection_id
        if (typeof connection === 'string' && meta.owner !== null) {
          grants.set(connection, meta)
          record('grant-observed', { connection, owner: meta.owner, generationOwner: meta.generationOwner, request: meta.request, parentIdentityKnown: Boolean(meta.control && meta.generation) })
        }
      })
    }
    return original
  }
  window.EventSource = new Proxy(NativeSource, {
    construct(target, args, newTarget) {
      const original = Reflect.construct(target, args, newTarget)
      try {
        const path = inspect(args[0])
        if (path?.category === 'events') {
          const grant = grants.get(path.connection)
          const meta = { connection: path.connection, source: ++sourceOrdinal, owner: grant?.owner ?? null, grantMatched: Boolean(grant) }
          sources.set(original, meta)
          sourceRecord('source-created', meta, original)
          for (const kind of ['open', 'error']) {
            original.addEventListener(kind, (event) => sourceRecord(`source-${kind}-event`, meta, original, {
              nativeTrusted: event.isTrusted === true && event instanceof ownerWindow.Event && event.target === original && event.currentTarget === original,
              nativeEventTime: event.timeStamp,
            }))
          }
        }
      }
      catch { observerFailures++ }
      return original
    },
  })
  NativeSource.prototype.close = function (...args) {
    const meta = sources.get(this)
    if (!meta)
      return Reflect.apply(close, this, args)
    sourceRecord('source-close-enter', meta, this)
    try {
      return Reflect.apply(close, this, args)
    }
    finally {
      sourceRecord('source-close-return', meta, this)
    }
  }
  for (const kind of ['beforeunload', 'pagehide']) {
    window.addEventListener(kind, (event) => {
      const constructor = kind === 'beforeunload' ? ownerWindow.BeforeUnloadEvent : ownerWindow.PageTransitionEvent
      record(kind, {
        nativeTrusted: typeof constructor === 'function' && event instanceof constructor && event.isTrusted === true && event.target === ownerDocument && event.currentTarget === ownerWindow,
        nativeEventTime: event.timeStamp,
      })
    })
  }
  window.addEventListener('error', (event) => {
    try {
      const matches = event.error && causes.get(event.error)
      if (matches?.length === 1) {
        const meta = matches[0]
        record('window-error-same-original-cause', { connection: meta.connection, owner: meta.owner, request: meta.request, category: meta.category, nativeTrusted: event.isTrusted === true, nativeEventTime: event.timeStamp })
      }
      else if (matches?.length > 1) {
        record('window-error-ambiguous-original-cause', { nativeTrusted: event.isTrusted === true, nativeEventTime: event.timeStamp })
      }
    }
    catch { observerFailures++ }
  })
  window[api] = {
    snapshot() {
      try { return { journal: JSON.parse(sessionStorage.getItem(key) || '[]'), observerFailures } }
      catch { return { journal: [], observerFailures: observerFailures + 1 } }
    },
  }
}

function errorIdentity(error) {
  try {
    const message = typeof error?.message === 'string' ? error.message : ''
    const match = /\/direct\/([^/?\s]+)\/(signal|events)(?:[?\s]|$)/.exec(message)
    return match ? { connection: decodeURIComponent(match[1]), category: match[2] } : null
  }
  catch { return null }
}

export function projectDirectOwners(journal, pageErrors, observerFailures = 0, closure) {
  const inputKnown = Array.isArray(journal) && Array.isArray(pageErrors) && Number.isSafeInteger(observerFailures) && observerFailures >= 0
  journal = Array.isArray(journal) ? journal.filter(value => value && typeof value === 'object') : []
  pageErrors = Array.isArray(pageErrors) ? pageErrors : []
  const identities = pageErrors.map(value => errorIdentity(value))
  const connections = [...new Set(identities.filter(Boolean).map(value => value.connection))]
  const connection = connections.length === 1 ? connections[0] : null
  const grants = journal.filter(event => connection !== null && event.kind === 'grant-observed' && event.connection === connection && event.agentsDocument === true && event.ownContext === true && event.parentIdentityKnown === true)
  const documents = [...new Set(grants.map(event => event.document).filter(value => typeof value === 'string'))]
  const document = documents.length === 1 ? documents[0] : null
  const owners = [...new Set(grants.map(event => event.owner).filter(Number.isInteger))]
  const owner = owners.length === 1 ? owners[0] : null
  const generations = [...new Set(grants.map(event => event.generationOwner).filter(Number.isInteger))]
  const generationOwner = generations.length === 1 ? generations[0] : null
  const related = journal.filter(event => document && event.document === document && (event.connection === connection || event.owner === owner || (generationOwner !== null && event.owner === generationOwner) || ['beforeunload', 'pagehide'].includes(event.kind)))
  const allowedKinds = ['grant-observed','control-start','control-headers','control-threw','control-rejected-reaction','request-abort-event','parent-abort-event','parent-abort-call-enter','parent-abort-call-return','source-created','source-open-event','source-error-event','source-close-enter','source-close-return','beforeunload','pagehide','window-error-same-original-cause']
  const booleans = ['agentsDocument','ownContext','parentIdentityKnown','requestSignalKnown','requestAborted','controlSignalKnown','controlAborted','generationSignalKnown','generationAborted','requestReasonMatches','controlReasonMatches','generationReasonMatches','nativeTrusted','insideOwnAbortCall','grantMatched','alreadyAborted','aborted']
  const events = related.map((event) => {
    const out = {
      kind: allowedKinds.includes(event.kind) ? event.kind : 'UNKNOWN',
      sequence: Number.isInteger(event.sequence) ? event.sequence : null,
      browserTime: Number.isFinite(event.browserTime) ? event.browserTime : null,
      nativeEventTime: Number.isFinite(event.nativeEventTime) ? event.nativeEventTime : null,
      category: ['authorize','signal','events'].includes(event.category) ? event.category : null,
      role: ['control','generation'].includes(event.role) ? event.role : null,
      causeClass: ['TypeError','AbortError','other'].includes(event.causeClass) ? event.causeClass : null,
      readyState: [0,1,2].includes(event.readyState) ? event.readyState : null,
      status: Number.isInteger(event.status) ? event.status : null,
    }
    for (const key of booleans)
      out[key] = typeof event[key] === 'boolean' ? event[key] : null
    return out
  })
  const rejected = events.filter(event => event.kind === 'control-rejected-reaction' && event.category === 'signal')
  const closed = events.filter(event => event.kind === 'source-close-return')
  const sourceCreators = related.filter(event => event.kind === 'source-created' && event.grantMatched === true && event.owner === owner && event.ownContext === true && Number.isInteger(event.source))
  const sourceIdentities = [...new Set(sourceCreators.map(event => event.source))]
  const source = sourceIdentities.length === 1 ? sourceIdentities[0] : null
  const ownedCloses = related.filter(event => source !== null && event.kind === 'source-close-return' && event.source === source && event.owner === owner && event.grantMatched === true && event.ownContext === true && event.readyState === 2)
  const structural = related.every(event => allowedKinds.includes(event.kind) && Number.isInteger(event.sequence) && event.sequence > 0 && event.ownContext === true) && new Set(related.map(event => event.sequence)).size === related.length
  const nativeErrors = events.filter(event => event.kind === 'window-error-same-original-cause' && event.nativeTrusted)
  const closureKnown = typeof closure?.pageClosed === 'boolean' && typeof closure?.gateClosed === 'boolean' && Number.isSafeInteger(closure?.cleanupFailureCount) && closure.cleanupFailureCount >= 0
  const firstErrorPairReproduced = identities.length === 2 && connections.length === 1 && identities.every(Boolean) && new Set(identities.map(value => value.category)).size === 2
  const sequence = predicate => {
    const values = events.filter(predicate)
    return values.length === 1 && Number.isInteger(values[0].sequence) ? values[0].sequence : null
  }
  const rejectionSequence = sequence(event => event.kind === 'control-rejected-reaction' && event.category === 'signal')
  const controlAbortSequence = sequence(event => event.kind === 'parent-abort-event' && event.role === 'control' && event.nativeTrusted && event.insideOwnAbortCall)
  const sourceErrorSequence = sequence(event => event.kind === 'source-error-event' && event.nativeTrusted)
  const sourceCloseSequence = sequence(event => event.kind === 'source-close-enter')
  return {
    diagnosticOnly: true,
    productQualified: false,
    nativeGenerationPublicationCorrelation: false,
    ownershipRoleBasis: 'Actual native any-input identities plus pinned documentSignal source order; not a Vue publication receipt',
    pageErrorCount: pageErrors.length,
    errorKinds: identities.map(value => value?.category ?? 'UNKNOWN'),
    errorConnectionUnique: connection !== null,
    actualGrantDocumentUnique: document !== null,
    actualControlOwnerUnique: owner !== null,
    actualGenerationParentUnique: generationOwner !== null,
    inputKnown,
    observerFailureCount: inputKnown ? observerFailures : 0,
    observerFailureCountScope: 'Observed instrumentation failures; missing data remains UNKNOWN',
    firstErrorPairReproduced,
    closureKnown,
    pageClosed: closureKnown ? closure.pageClosed : null,
    gateClosed: closureKnown ? closure.gateClosed : null,
    cleanupFailureCount: closureKnown ? closure.cleanupFailureCount : null,
    originalControlRejectionObserved: rejected.length === 1,
    sourceCloseObserved: closed.length === 1,
    actualSourceUnique: source !== null,
    sourceCloseProvenanceKnown: ownedCloses.length === 1,
    relatedJournalStructurallyKnown: structural,
    ownerReactionStateKnown: rejected.length === 1 && rejected[0].controlSignalKnown === true && rejected[0].generationSignalKnown === true,
    lifecycleObserved: events.some(event => event.kind === 'beforeunload' && event.nativeTrusted) && events.some(event => event.kind === 'pagehide' && event.nativeTrusted),
    ambiguousOriginalCauseErrorCount: journal.filter(event => event.kind === 'window-error-ambiguous-original-cause' && event.document === document).length,
    nativeErrorObjectCorrespondence: nativeErrors.length > 0 ? 'SAME_ORIGINAL_CAUSE_EVENT_OBSERVED' : 'UNKNOWN',
    browserLocalOwnerOrdering: {
      controlAbortBeforeOriginalRejectionReaction: controlAbortSequence !== null && rejectionSequence !== null ? controlAbortSequence < rejectionSequence : null,
      nativeSourceErrorBeforeClose: sourceErrorSequence !== null && sourceCloseSequence !== null ? sourceErrorSequence < sourceCloseSequence : null,
    },
    nativeOrderingAgainstBothPageErrors: 'UNKNOWN',
    orderingLimit: 'Browser-local sequence is real observation order. Host pageerror delivery is not native error creation; no cross-clock ordering is inferred.',
    receiptStatus: inputKnown && observerFailures === 0 && firstErrorPairReproduced && closureKnown && closure.pageClosed && closure.gateClosed && closure.cleanupFailureCount === 0 && structural && document !== null && owner !== null && generationOwner !== null && rejected.length === 1 && rejected[0].controlSignalKnown === true && rejected[0].generationSignalKnown === true && ownedCloses.length === 1 ? 'PARTIAL' : 'UNKNOWN',
    events,
  }
}

export function createDirectOwnerObserver(page) {
  const journal = new Map()
  const pageErrors = []
  let failures = 0
  const onError = (error) => {
    try { pageErrors.push({ message: error.message, hostTime: performance.now() }) }
    catch { failures++ }
  }
  let listenerAttached = false
  return {
    async install() {
      if (listenerAttached)
        throw new Error('Direct owner observer installed twice')
      page.on('pageerror', onError)
      listenerAttached = true
      await closedTargetBound(() => page.exposeFunction('__c40RecordDirectOwner', (event) => {
        if (!event || typeof event.document !== 'string' || !Number.isInteger(event.sequence)) {
          failures++
          return
        }
        journal.set(`${event.document}:${event.sequence}`, { ...event, hostIngress: performance.now() })
      }), 'direct owner binding')
      await closedTargetBound(() => page.addInitScript(observeDirectOwners), 'direct owner bootstrap')
    },
    accept(snapshot) {
      if (!snapshot || !Array.isArray(snapshot.journal) || !Number.isSafeInteger(snapshot.observerFailures) || snapshot.observerFailures < 0) {
        failures++
        return
      }
      failures += snapshot.observerFailures
      for (const event of snapshot.journal) {
        if (!event || typeof event.document !== 'string' || !Number.isInteger(event.sequence)) {
          failures++
          continue
        }
        const key = `${event.document}:${event.sequence}`
        journal.set(key, { ...event, hostIngress: journal.get(key)?.hostIngress ?? null })
      }
    },
    finish(closure) {
      return {
        privateEvidence: { journal: [...journal.values()], pageErrors, observerFailures: failures },
        projection: projectDirectOwners([...journal.values()], pageErrors, failures, closure),
      }
    },
    dispose() {
      if (!listenerAttached)
        return true
      try {
        page.off('pageerror', onError)
        listenerAttached = false
        return true
      }
      catch { failures++; return false }
    },
  }
}
