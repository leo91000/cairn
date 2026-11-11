import { performance } from 'node:perf_hooks'
import { isDeepStrictEqual } from 'node:util'
import { closedTargetBound } from './closed-target-error.mjs'

// Serialized into the real page. Ownership association requires the reviewed
// pinned producer/delegate chain; this code does not detect arbitrary Proxies.
// All identities below stay private; publication
// is exclusively through projectDirectOwners. No original promise is replaced.
export function observeDirectOwners(installationScope) {
  'use strict'
  const key = '__c40_direct_owner_journal_v1'
  const api = '__c40DirectOwnerReceipt'
  if (Object.hasOwn(window, api))
    return
  const ownerWindow = window
  const ownerDocument = document
  const fetch = window.fetch
  const json = window.Response.prototype.json
  const arrayPrototype = window.Array.prototype
  const arrayIterator = Object.getOwnPropertyDescriptor(arrayPrototype, Symbol.iterator)?.value
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
  const grantKey = (installation, connection) => JSON.stringify([installation, connection])
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
      const route = /^\/installations\/([^/]+)\/agents$/.exec(location.pathname)
      const routeIdentity = route ? decodeURIComponent(route[1]) : null
      const mappingKnown = typeof installationScope?.installationId === 'string' && installationScope.installationId.length > 0 && typeof installationScope?.installationSlug === 'string' && installationScope.installationSlug.length > 0
      const documentInstallation = mappingKnown && (routeIdentity === installationScope.installationSlug || routeIdentity === installationScope.installationId) ? installationScope.installationId : null
      const event = {
        kind,
        sequence: journal.length + 1,
        document: typeof window.cairnAvailabilityDocument === 'string' ? window.cairnAvailabilityDocument : null,
        agentsDocument: documentInstallation !== null,
        documentInstallation,
        documentInstallationMappingKnown: documentInstallation !== null,
        installationMatchesDocument: typeof detail.installation === 'string' && documentInstallation !== null ? detail.installation === documentInstallation : null,
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
  const ordinaryData = (value) => {
    if (value == null)
      return Object.create(null)
    if (typeof value !== 'object')
      return null
    const prototype = Object.getPrototypeOf(value)
    if (prototype !== null && prototype !== Object.prototype)
      return null
    if (prototype) {
      const standard = ['constructor', '__defineGetter__', '__defineSetter__', 'hasOwnProperty', '__lookupGetter__', '__lookupSetter__', 'isPrototypeOf', 'propertyIsEnumerable', 'toString', 'valueOf', '__proto__', 'toLocaleString']
      const inherited = Object.getOwnPropertyDescriptors(prototype)
      Object.setPrototypeOf(inherited, null)
      for (const key of Reflect.ownKeys(inherited)) {
        if (typeof key !== 'string' || !standard.includes(key))
          return null
        const descriptor = Object.getOwnPropertyDescriptor(inherited, key).value
        if (key !== '__proto__' && !Object.hasOwn(descriptor, 'value'))
          return null
      }
    }
    const descriptors = Object.getOwnPropertyDescriptors(value)
    Object.setPrototypeOf(descriptors, null)
    const data = Object.create(null)
    for (const key of Reflect.ownKeys(descriptors)) {
      if (typeof key !== 'string')
        return null
      const descriptor = Object.getOwnPropertyDescriptor(descriptors, key).value
      if (!Object.hasOwn(descriptor, 'value'))
        return null
      data[key] = descriptor.value
    }
    return data
  }
  const requestData = (value) => {
    const data = ordinaryData(value)
    if (!data)
      return null
    for (const [key, item] of Object.entries(data)) {
      if (key === 'signal')
        continue
      if (key === 'headers' && item != null) {
        const headers = ordinaryData(item)
        if (!headers || Object.values(headers).some(value => typeof value !== 'string'))
          return null
      }
      else if (item !== null && (typeof item === 'object' || typeof item === 'function'))
        return null
    }
    return data
  }

  const inspect = (value) => {
    if (typeof value !== 'string')
      return null
    const url = new URL(value, location.href)
    if (url.origin !== location.origin)
      return null
    const match = /^\/api\/installations\/([^/]+)\/direct\/(authorize|([^/]+)\/(signal|events))$/.exec(url.pathname)
    if (!match)
      return null
    return { installation: decodeURIComponent(match[1]), category: match[2] === 'authorize' ? 'authorize' : match[4], connection: match[3] ? decodeURIComponent(match[3]) : null }
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
    let parents
    try {
      const values = args[0]
      if (Array.isArray(values) && Object.getPrototypeOf(values) === arrayPrototype) {
        const iterator = Object.getOwnPropertyDescriptor(values, Symbol.iterator)
        const inherited = Object.getOwnPropertyDescriptor(arrayPrototype, Symbol.iterator)
        const length = Object.getOwnPropertyDescriptor(values, 'length')
        const first = Object.getOwnPropertyDescriptor(values, '0')
        const second = Object.getOwnPropertyDescriptor(values, '1')
        if (!iterator && arrayIterator && inherited && Object.hasOwn(inherited, 'value') && inherited.value === arrayIterator
          && length?.value === 2 && first && second && Object.hasOwn(first, 'value') && Object.hasOwn(second, 'value')
          && first.value instanceof window.AbortSignal && second.value instanceof window.AbortSignal && first.value !== second.value) {
          parents = { generation: first.value, control: second.value }
        }
      }
    }
    catch { observerFailures++ }
    // Validate supported iteration BEFORE the native call can remove/mutate it.
    // Do not consume any iterator for observation; return the ORIGINAL result.
    const result = Reflect.apply(any, this, args)
    try {
      if (parents)
        parentSets.set(result, parents)
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
    try {
      try { record('parent-abort-call-enter', { ...owner, alreadyAborted: Reflect.apply(abortedGetter, signal, []) }) }
      catch { observerFailures++ }
      return Reflect.apply(abort, this, args)
    }
    finally {
      try { record('parent-abort-call-return', { ...owner, aborted: Reflect.apply(abortedGetter, signal, []) }) }
      catch { observerFailures++ }
      const index = abortCalls.lastIndexOf(call)
      if (index >= 0)
        abortCalls.splice(index, 1)
      else
        observerFailures++
    }
  }
  const controlIdentity = meta => ({ installation: meta.installation, category: meta.category, connection: meta.connection, request: meta.request, owner: meta.owner, generationOwner: meta.generationOwner, method: meta.method })
  window.fetch = function (...args) {
    if (this != null && this !== ownerWindow)
      return Reflect.apply(fetch, this, args)
    let meta
    try {
      const path = inspect(args[0])
      const data = path ? requestData(args[1]) : null
      if (path && !data)
        observerFailures++
      const signal = data?.signal
      const parents = signal && parentSets.get(signal)
      if (path && path.category !== 'events') {
        const control = parents?.control
        const generation = parents?.generation
        meta = { ...path, request: ++requestOrdinal, signal, control, generation, method: data && data.method === undefined ? 'GET' : typeof data?.method === 'string' ? data.method : null, owner: parents ? trackParent(control, 'control').owner : null }
        meta.generationOwner = parents ? trackParent(generation, 'generation').owner : null
        if (signal instanceof window.AbortSignal) {
          signal.addEventListener('abort', (event) => record('request-abort-event', {
            ...controlIdentity(meta),
            nativeTrusted: event.isTrusted === true && event instanceof ownerWindow.Event && event.target === signal && event.currentTarget === signal,
            insideOwnAbortCall: Boolean(parents && abortCalls.some(call => call.signal === control || call.signal === generation)),
            nativeEventTime: event.timeStamp,
          }), { once: true })
        }
        record('control-start', { ...controlIdentity(meta), parentIdentityKnown: Boolean(parents), ...state(signal, control, generation) })
      }
    }
    catch { observerFailures++ }
    let original
    try { original = Reflect.apply(fetch, this, args) }
    catch (cause) {
      if (meta)
        record('control-threw', { ...controlIdentity(meta), ...state(meta.signal, meta.control, meta.generation, cause) })
      throw cause
    }
    if (meta) {
      observe(original, (response) => {
        responses.set(response, meta)
        record('control-headers', { ...controlIdentity(meta), status: response.status })
      }, (cause) => {
        if (cause && (typeof cause === 'object' || typeof cause === 'function'))
          causes.set(cause, [...(causes.get(cause) ?? []), meta])
        record('control-rejected-reaction', {
          ...controlIdentity(meta),
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
        if (typeof connection === 'string' && meta.owner !== null && meta.method === 'POST') {
          grants.set(grantKey(meta.installation, connection), meta)
          record('grant-observed', { installation: meta.installation, connection, owner: meta.owner, generationOwner: meta.generationOwner, request: meta.request, method: meta.method, parentIdentityKnown: Boolean(meta.control && meta.generation) })
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
          const grant = grants.get(grantKey(path.installation, path.connection))
          const meta = { installation: path.installation, connection: path.connection, source: ++sourceOrdinal, owner: grant?.owner ?? null, generationOwner: grant?.generationOwner ?? null, grantMatched: Boolean(grant) }
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
      const original = Reflect.apply(close, this, args)
      sourceRecord('source-close-return', meta, this)
      return original
    }
    catch (cause) {
      sourceRecord('source-close-threw', meta, this)
      throw cause
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
        record('window-error-same-original-cause', { ...controlIdentity(meta), nativeTrusted: event.isTrusted === true, nativeEventTime: event.timeStamp })
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
    const match = /\/api\/installations\/([^/?\s]+)\/direct\/([^/?\s]+)\/(signal|events)(?:[?\s]|$)/.exec(message)
    return match ? { installation: decodeURIComponent(match[1]), connection: decodeURIComponent(match[2]), category: match[3] } : null
  }
  catch { return null }
}

export function projectDirectOwners(journal, pageErrors, observerFailures = 0, closure) {
  const inputKnown = Array.isArray(journal) && Array.isArray(pageErrors) && Number.isSafeInteger(observerFailures) && observerFailures >= 0
  journal = Array.isArray(journal) ? journal.filter(value => value && typeof value === 'object') : []
  pageErrors = Array.isArray(pageErrors) ? pageErrors : []
  const identities = pageErrors.map(value => errorIdentity(value))
  const connections = [...new Set(identities.filter(Boolean).map(value => value.connection))]
  const installations = [...new Set(identities.filter(Boolean).map(value => value.installation))]
  const installation = installations.length === 1 ? installations[0] : null
  const connection = installation !== null && connections.length === 1 ? connections[0] : null
  const grants = journal.filter(event => connection !== null && event.kind === 'grant-observed' && event.installation === installation && event.installationMatchesDocument === true && event.method === 'POST' && event.connection === connection && event.agentsDocument === true && event.ownContext === true && event.parentIdentityKnown === true)
  const documents = [...new Set(grants.map(event => event.document).filter(value => typeof value === 'string'))]
  const document = documents.length === 1 ? documents[0] : null
  const owners = [...new Set(grants.map(event => event.owner).filter(Number.isInteger))]
  const owner = owners.length === 1 ? owners[0] : null
  const generations = [...new Set(grants.map(event => event.generationOwner).filter(Number.isInteger))]
  const generationOwner = generations.length === 1 ? generations[0] : null
  const related = journal.filter(event => document && event.document === document && (event.connection === connection || event.owner === owner || (generationOwner !== null && event.owner === generationOwner) || ['beforeunload', 'pagehide'].includes(event.kind)))
  const allowedKinds = ['grant-observed','control-start','control-headers','control-threw','control-rejected-reaction','request-abort-event','parent-abort-event','parent-abort-call-enter','parent-abort-call-return','source-created','source-open-event','source-error-event','source-close-enter','source-close-return','source-close-threw','beforeunload','pagehide','window-error-same-original-cause']
  const booleans = ['agentsDocument','ownContext','documentInstallationMappingKnown','installationMatchesDocument','parentIdentityKnown','requestSignalKnown','requestAborted','controlSignalKnown','controlAborted','generationSignalKnown','generationAborted','requestReasonMatches','controlReasonMatches','generationReasonMatches','nativeTrusted','insideOwnAbortCall','grantMatched','alreadyAborted','aborted']
  const events = related.map((event) => {
    const out = {
      kind: allowedKinds.includes(event.kind) ? event.kind : 'UNKNOWN',
      sequence: Number.isInteger(event.sequence) ? event.sequence : null,
      browserTime: Number.isFinite(event.browserTime) ? event.browserTime : null,
      nativeEventTime: Number.isFinite(event.nativeEventTime) ? event.nativeEventTime : null,
      category: ['authorize','signal','events'].includes(event.category) ? event.category : null,
      method: ['POST', 'GET'].includes(event.method) ? event.method : event.method === undefined ? null : 'OTHER',
      installationMatchesSelected: typeof event.installation === 'string' ? event.installation === installation : null,
      generationMatchesGrant: Number.isInteger(event.generationOwner) ? event.generationOwner === generationOwner : null,
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
  const privateRejections = related.filter(event => event.kind === 'control-rejected-reaction' && event.category === 'signal' && event.connection === connection)
  const selectedRejection = privateRejections.length === 1 ? privateRejections[0] : null
  const rejectionStarts = selectedRejection ? related.filter(event => event.kind === 'control-start' && event.category === 'signal' && event.connection === connection && event.owner === owner && event.generationOwner === generationOwner && event.installation === installation && event.installationMatchesDocument === true && event.method === 'POST' && event.request === selectedRejection.request && event.sequence < selectedRejection.sequence && event.parentIdentityKnown === true) : []
  const rejectionIdentityKnown = grants.length === 1 && selectedRejection !== null && selectedRejection.owner === owner && selectedRejection.generationOwner === generationOwner && selectedRejection.installation === installation && selectedRejection.installationMatchesDocument === true && selectedRejection.method === 'POST' && Number.isSafeInteger(selectedRejection.request) && rejectionStarts.length === 1
  const closed = events.filter(event => event.kind === 'source-close-return')
  // First count EVERY source on this document/connection. Filtering by an
  // already-matched owner would conceal the very ambiguity this receipt tests.
  const sourceCreators = related.filter(event => event.kind === 'source-created' && event.connection === connection)
  const source = sourceCreators.length === 1 && Number.isSafeInteger(sourceCreators[0].source) ? sourceCreators[0].source : null
  const creatorKnown = source !== null && sourceCreators[0].grantMatched === true && sourceCreators[0].owner === owner && sourceCreators[0].generationOwner === generationOwner && sourceCreators[0].installation === installation && sourceCreators[0].installationMatchesDocument === true && sourceCreators[0].ownContext === true
  const closeEntries = related.filter(event => event.kind === 'source-close-enter' && event.connection === connection)
  const allCloses = related.filter(event => event.kind === 'source-close-return' && event.connection === connection)
  const ownedCloses = creatorKnown && closeEntries.length === 1 && allCloses.length === 1
    ? allCloses.filter(event => event.source === source && closeEntries[0].source === source && event.installation === installation && closeEntries[0].installation === installation && event.generationOwner === generationOwner && closeEntries[0].generationOwner === generationOwner && event.installationMatchesDocument === true && closeEntries[0].installationMatchesDocument === true && event.owner === owner && closeEntries[0].owner === owner && event.grantMatched === true && closeEntries[0].grantMatched === true && event.ownContext === true && event.readyState === 2 && closeEntries[0].sequence < event.sequence)
    : []
  const structural = related.every(event => allowedKinds.includes(event.kind) && Number.isInteger(event.sequence) && event.sequence > 0 && event.ownContext === true) && new Set(related.map(event => event.sequence)).size === related.length
  const documentJournal = journal.filter(event => document !== null && event.document === document)
  const orderedSequences = documentJournal.map(event => event.sequence).sort((a, b) => a - b)
  const documentJournalContinuous = orderedSequences.length > 0 && orderedSequences.every((value, index) => Number.isSafeInteger(value) && value > 0 && (index === 0 || value === orderedSequences[index - 1] + 1))
  const unloads = related.filter(event => event.kind === 'beforeunload')
  const hides = related.filter(event => event.kind === 'pagehide')
  const lifecycleObserved = unloads.length === 1 && hides.length === 1
    && unloads[0].nativeTrusted === true && hides[0].nativeTrusted === true
    && unloads[0].ownContext === true && hides[0].ownContext === true
    && unloads[0].sequence < hides[0].sequence
  const nativeErrors = events.filter(event => event.kind === 'window-error-same-original-cause' && event.nativeTrusted)
  const closureKnown = typeof closure?.pageClosed === 'boolean' && typeof closure?.gateClosed === 'boolean' && Number.isSafeInteger(closure?.cleanupFailureCount) && closure.cleanupFailureCount >= 0
  const firstErrorPairReproduced = identities.length === 2 && installations.length === 1 && connections.length === 1 && identities.every(Boolean) && new Set(identities.map(value => value.category)).size === 2
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
    ownershipEvidenceScope: 'PINNED_SOURCE_CONDITIONAL',
    universalRuntimeAdmission: false,
    runtimeProducerReceipt: false,
    proxyDetection: 'NOT_PROVIDED',
    nativeGenerationPublicationCorrelation: false,
    installationScopeBasis: 'Pinned Workspace fixture installationId/installationSlug binding plus current document route; no slug-as-UUID inference or producer receipt',
    ownershipRoleBasis: 'Conditional association for the reviewed pinned producers and complete delegate chain; descriptor agreement is not universal runtime membership or a Vue publication receipt',
    pageErrorCount: pageErrors.length,
    errorKinds: identities.map(value => value?.category ?? 'UNKNOWN'),
    errorConnectionUnique: connection !== null,
    errorInstallationUnique: installation !== null,
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
    rejectionIdentityKnown,
    sourceCloseObserved: closed.length === 1,
    actualSourceUnique: source !== null,
    sourceCloseProvenanceKnown: ownedCloses.length === 1,
    relatedJournalStructurallyKnown: structural,
    ownerReactionStateKnown: rejectionIdentityKnown && rejected.length === 1 && rejected[0].requestSignalKnown === true && rejected[0].controlSignalKnown === true && rejected[0].generationSignalKnown === true,
    documentJournalContinuous,
    lifecycleObserved,
    ambiguousOriginalCauseErrorCount: journal.filter(event => event.kind === 'window-error-ambiguous-original-cause' && event.document === document).length,
    nativeErrorObjectCorrespondence: nativeErrors.length > 0 ? 'SAME_ORIGINAL_CAUSE_EVENT_OBSERVED' : 'UNKNOWN',
    browserLocalOwnerOrdering: {
      controlAbortBeforeOriginalRejectionReaction: controlAbortSequence !== null && rejectionSequence !== null ? controlAbortSequence < rejectionSequence : null,
      nativeSourceErrorBeforeClose: sourceErrorSequence !== null && sourceCloseSequence !== null ? sourceErrorSequence < sourceCloseSequence : null,
    },
    nativeOrderingAgainstBothPageErrors: 'UNKNOWN',
    orderingLimit: 'Browser-local sequence is real observation order. Host pageerror delivery is not native error creation; no cross-clock ordering is inferred.',
    receiptStatus: inputKnown && observerFailures === 0 && firstErrorPairReproduced && closureKnown && closure.pageClosed && closure.gateClosed && closure.cleanupFailureCount === 0 && structural && documentJournalContinuous && lifecycleObserved && document !== null && owner !== null && generationOwner !== null && rejectionIdentityKnown && rejected.length === 1 && rejected[0].requestSignalKnown === true && rejected[0].controlSignalKnown === true && rejected[0].generationSignalKnown === true && ownedCloses.length === 1 ? 'SOURCE_CONDITIONAL_PARTIAL' : 'UNKNOWN',
    events,
  }
}

export function createDirectOwnerObserver(page, installationScope) {
  if (!installationScope || typeof installationScope.installationId !== 'string' || !installationScope.installationId || typeof installationScope.installationSlug !== 'string' || !installationScope.installationSlug)
    throw new Error('Source-trusted installation mapping unavailable')
  const boundScope = Object.freeze({ installationId: installationScope.installationId, installationSlug: installationScope.installationSlug })
  const journal = new Map()
  const pageErrors = []
  const conflicts = []
  let failures = 0
  const acceptEvent = (event, ingress) => {
    try {
      if (!event || typeof event.document !== 'string' || !Number.isSafeInteger(event.sequence) || event.sequence <= 0) {
        failures++
        return
      }
      const received = structuredClone(event)
      const key = JSON.stringify([received.document, received.sequence])
      const prior = journal.get(key)
      if (!prior) {
        journal.set(key, { event: received, hostIngress: ingress })
      }
      else if (!isDeepStrictEqual(prior.event, received)) {
        conflicts.push({ event: received, hostIngress: ingress })
        failures++
      }
      // Identical retransmission is idempotent; never overwrite first evidence.
    }
    catch { failures++ }
  }
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
        acceptEvent(event, performance.now())
      }), 'direct owner binding')
      await closedTargetBound(() => page.addInitScript(observeDirectOwners, boundScope), 'direct owner bootstrap')
    },
    accept(snapshot) {
      if (!snapshot || !Array.isArray(snapshot.journal) || !Number.isSafeInteger(snapshot.observerFailures) || snapshot.observerFailures < 0) {
        failures++
        return
      }
      failures += snapshot.observerFailures
      for (const event of snapshot.journal)
        acceptEvent(event, null)
    },
    finish(closure) {
      return {
        privateEvidence: { journal: [...journal.values()].map(value => ({ ...value.event, hostIngress: value.hostIngress })), conflicts, pageErrors, observerFailures: failures },
        projection: projectDirectOwners([...journal.values()].map(value => value.event), pageErrors, failures, closure),
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
