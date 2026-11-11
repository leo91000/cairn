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
  const failureConsole = window.console
  const failureConsoleError = failureConsole.error
  const failureSignal = '__c40_direct_observer_failure_v1__'
  const failureKey = key + ':observer-failure'
  // Sticky same-origin failure evidence survives a new bootstrap closure.
  // The numeric snapshot is a conservative lower bound, never proof of no loss.
  const noteObserverFailure = () => {
    observerFailures++
    try { sessionStorage.setItem(failureKey, '1') }
    catch { /* Host-owned failure ingress below does not depend on storage. */ }
    // One closed diagnostic code only: no event payload, URL or error text.
    // Console protocol is independent of both the page binding and storage.
    try { Reflect.apply(failureConsoleError, failureConsole, [failureSignal]) }
    catch { /* Unsupported console still leaves local/sticky failure evidence. */ }
  }
  try {
    const prior = sessionStorage.getItem(failureKey)
    if (prior === null) {
      const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
      if (!Array.isArray(journal) || journal.length > 0)
        noteObserverFailure()
      else
        sessionStorage.setItem(failureKey, '0')
    }
    else if (prior === '1') {
      observerFailures = 1
    }
    else if (prior !== '0') {
      noteObserverFailure()
    }
  }
  catch { noteObserverFailure() }
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
      void Reflect.apply(then, pending, [undefined, () => { noteObserverFailure() }])
    }
    catch {
      noteObserverFailure()
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
        value => { try { fulfilled?.(value) } catch { noteObserverFailure() } },
        cause => { try { rejected?.(cause) } catch { noteObserverFailure() } },
      ])
      void Reflect.apply(then, observation, [undefined, () => { noteObserverFailure() }])
    }
    catch { noteObserverFailure() }
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
    catch { noteObserverFailure() }
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
    catch { noteObserverFailure() }
    // Validate supported iteration BEFORE the native call can remove/mutate it.
    // Do not consume any iterator for observation; return the ORIGINAL result.
    const result = Reflect.apply(any, this, args)
    try {
      if (parents)
        parentSets.set(result, parents)
    }
    catch { noteObserverFailure() }
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
      catch { noteObserverFailure() }
      return Reflect.apply(abort, this, args)
    }
    finally {
      try { record('parent-abort-call-return', { ...owner, aborted: Reflect.apply(abortedGetter, signal, []) }) }
      catch { noteObserverFailure() }
      const index = abortCalls.lastIndexOf(call)
      if (index >= 0)
        abortCalls.splice(index, 1)
      else
        noteObserverFailure()
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
        noteObserverFailure()
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
    catch { noteObserverFailure() }
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
      catch { noteObserverFailure() }
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
    catch { noteObserverFailure() }
  })
  window[api] = {
    snapshot() {
      try {
        const prior = sessionStorage.getItem(failureKey)
        if (prior !== '0' && prior !== '1')
          throw new Error('Observer failure evidence UNKNOWN')
        return { journal: JSON.parse(sessionStorage.getItem(key) || '[]'), observerFailures: Math.max(observerFailures, prior === '1' ? 1 : 0) }
      }
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

function selectDirectOwnerIdentity(journal, pageErrors) {
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
  return { identities, installations, connections, installation, connection, grants, document, owner, generationOwner }
}

export function projectDirectOwners(journal, pageErrors, observerFailures = 0, closure) {
  const inputKnown = Array.isArray(journal) && Array.isArray(pageErrors) && Number.isSafeInteger(observerFailures) && observerFailures >= 0
  journal = Array.isArray(journal) ? journal.filter(value => value && typeof value === 'object') : []
  pageErrors = Array.isArray(pageErrors) ? pageErrors : []
  const { identities, installations, connections, installation, connection, grants, document, owner, generationOwner } = selectDirectOwnerIdentity(journal, pageErrors)
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
    nativeOperationEvidenceScope: 'PINNED_SOURCE_CONDITIONAL',
    universalNativeOperationEquivalence: false,
    outOfDomainOperationEquivalence: 'NOT_CLAIMED',
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

// HOST ONLY. Public SDK objects stay private and are never serialized or mutated.
function firstSseSdkCapture(page, scope, fail) {
  const requests = new Map()
  const documents = new Map()
  let ordinal = 0
  let metadataFailures = 0
  let orphanTerminals = 0
  let attached = false
  let closed = false
  const failedMetadata = () => { metadataFailures++; fail() }
  const routeMatches = (value) => {
    const route = /^\/installations\/([^/]+)\/agents$/.exec(value.pathname)
    return Boolean(route && decodeURIComponent(route[1]) === scope.installationSlug)
  }
  const identify = (request) => {
    const value = request.url()
    if (typeof value !== 'string')
      throw new Error('SDK Request URL metadata unavailable')
    const url = new URL(value)
    const match = /^\/api\/installations\/([^/]+)\/direct\/([^/]+)\/events$/.exec(url.pathname)
    if (!match)
      return null
    const installation = decodeURIComponent(match[1])
    if (installation !== scope.installationId)
      return null
    return { installation, connection: decodeURIComponent(match[2]), origin: url.origin }
  }
  const guest = (event) => {
    if (!['grant-observed', 'source-created'].includes(event.kind) || event.agentsDocument !== true || event.ownContext !== true || typeof page.mainFrame !== 'function')
      return
    try {
      const frame = page.mainFrame()
      const value = new URL(frame.url())
      // Late journal harvest from a replacement route cannot invent a Doc origin.
      if (!routeMatches(value))
        return
      const prior = documents.get(event.document)
      if (prior && (prior.frame !== frame || prior.origin !== value.origin)) {
        prior.conflicting = true
        failedMetadata()
      }
      else if (!prior) {
        documents.set(event.document, { frame, origin: value.origin, conflicting: false })
      }
    }
    catch { failedMetadata() }
  }
  const request = (value) => {
    try {
      const identity = identify(value)
      if (!identity)
        return
      if (requests.has(value)) {
        failedMetadata()
        return
      }
      const row = { ordinal: ++ordinal, ...identity, method: value.method(), resourceType: value.resourceType(), responseObserved: false, responseStatus: null, requestFailedObserved: false, requestFinishedObserved: false, terminalEvents: 0, failureTextPresent: false, errorTextCategory: 'NOT_OBSERVED', redirected: value.redirectedFrom() !== null, frameKnown: false, frameMatchesMain: false, agentsRoute: false }
      requests.set(value, row)
      const frame = value.frame()
      const url = new URL(frame.url())
      row.frame = frame
      row.frameKnown = true
      row.frameMatchesMain = frame === page.mainFrame()
      row.agentsRoute = routeMatches(url)
      row.frameOriginMatchesRequest = url.origin === identity.origin
    }
    catch { failedMetadata() }
  }
  const known = (value) => {
    const row = requests.get(value)
    if (row)
      return row
    // A response/terminal without its Request event cannot become a first SSE.
    if (identify(value)) {
      orphanTerminals++
      failedMetadata()
    }
    return null
  }
  const response = (value) => {
    try {
      const row = known(value.request())
      if (!row)
        return
      row.responseEventObserved = true
      const status = value.status()
      if (!Number.isInteger(status) || status < 100 || status > 599 || row.responseObserved)
        throw new Error('SDK response metadata inconsistent')
      row.responseObserved = true
      row.responseStatus = status
      row.redirected ||= status >= 300 && status < 400
    }
    catch { failedMetadata() }
  }
  const terminal = (value, kind) => {
    try {
      const row = known(value)
      if (!row)
        return
      row.requestFailedObserved ||= kind === 'requestfailed'
      row.requestFinishedObserved ||= kind === 'requestfinished'
      row.terminalEvents++
      if (row.terminalEvents !== 1)
        throw new Error('SDK request has duplicate terminal events')
      row.redirected ||= value.redirectedTo() !== null
      if (kind === 'requestfinished') {
        row.requestFinishedObserved = true
        return
      }
      row.requestFailedObserved = true
      const failure = value.failure()
      const text = failure?.errorText
      if (typeof text !== 'string' || text.length === 0) {
        row.errorTextCategory = 'SDK_FAILURE_TEXT_MISSING'
        return
      }
      row.failureTextPresent = true
      row.sdkErrorText = text // PRIVATE evidence only; never projected or coerced.
      const normalized = text.trim().toLowerCase()
      row.errorTextCategory = ['cancelled', 'canceled', 'net::err_aborted'].includes(normalized)
        ? 'SDK_CANCEL_REPORTED'
        : /cors|access[ -]control/.test(normalized) ? 'SDK_ACCESS_CONTROL_REPORTED' : 'SDK_OTHER_FAILURE'
    }
    catch { failedMetadata() }
  }
  const project = (journal, pageErrors, closure, totalFailures) => {
    const id = selectDirectOwnerIdentity(journal, pageErrors)
    const grants = journal.filter(e => e.kind === 'grant-observed' && e.installation === id.installation && e.connection === id.connection)
    const creators = journal.filter(e => e.kind === 'source-created' && e.installation === id.installation && e.connection === id.connection)
    const creator = creators.length === 1 ? creators[0] : null
    const sourceKnown = id.grants.length === 1 && grants.length === 1 && creator !== null && id.document !== null && id.owner !== null && id.generationOwner !== null
      && creator.document === id.document && creator.owner === id.owner && creator.generationOwner === id.generationOwner && creator.grantMatched === true && creator.ownContext === true && creator.installationMatchesDocument === true && Number.isSafeInteger(creator.source)
    const candidates = [...requests.values()].filter(r => id.connection !== null && r.connection === id.connection && r.installation === id.installation)
    const row = candidates.length === 1 ? candidates[0] : null
    const doc = documents.get(id.document)
    const requestKnown = sourceKnown && row !== null && row.method === 'GET' && row.resourceType === 'eventsource' && row.frameKnown && row.frameMatchesMain && row.agentsRoute && row.frameOriginMatchesRequest && doc !== undefined && !doc.conflicting && doc.frame === row.frame && doc.origin === row.origin && !row.redirected
    const captureClosed = attached && closed
    const closureKnown = captureClosed && closure?.pageClosed === true && closure?.gateClosed === true && closure?.cleanupFailureCount === 0
    const observationKnown = requestKnown && metadataFailures === 0 && orphanTerminals === 0 && totalFailures === 0 && closureKnown
    const terminalKnown = observationKnown && row.terminalEvents === 1 && (row.requestFinishedObserved || (row.requestFailedObserved && row.failureTextPresent))
    return {
      hostCollectorVersion: 'DIRECT_OWNER_HOST_V2_PUBLIC_SDK_FIRST_SSE',
      receiptOrigin: 'PUBLIC_PLAYWRIGHT_PAGE_EVENTS_AND_UNIQUE_PINNED_GUEST_REFERENCE_JOIN',
      correlationScope: 'PINNED_SOURCE_CONDITIONAL',
      requestObjectIdentityUsed: true,
      sourceMappingKnown: Boolean(sourceKnown),
      sameDocument: observationKnown ? true : null,
      sameGrantConnection: observationKnown ? true : null,
      sameSource: observationKnown ? true : null,
      sdkCandidateRequestCount: candidates.length,
      nativeFirstRequestExposureGuaranteed: false,
      sdkNativeDocumentIdentityExposed: false,
      firstObservedSdkRequestIsUnique: requestKnown ? true : null,
      redirectObserved: row ? row.redirected : null,
      frameKnown: row ? row.frameKnown : null,
      captureClosed, closureKnown,
      metadataFailureCount: metadataFailures, orphanTerminalCount: orphanTerminals,
      sdkResponseEventObserved: requestKnown ? row.responseEventObserved === true : null,
      responseObserved: requestKnown ? row.responseObserved : null,
      responseStatus: requestKnown && row.responseObserved ? row.responseStatus : null,
      requestFailedObserved: requestKnown ? row.requestFailedObserved : null,
      requestFinishedObserved: requestKnown ? row.requestFinishedObserved : null,
      sdkTerminalEvent: terminalKnown ? row.requestFailedObserved ? 'requestfailed' : 'requestfinished' : 'UNKNOWN',
      sdkErrorTextCategory: requestKnown ? row.errorTextCategory : 'UNKNOWN',
      sdkCancellationReport: observationKnown && row.requestFailedObserved && row.errorTextCategory === 'SDK_CANCEL_REPORTED' ? 'KNOWN_FROM_SDK_TEXT' : 'UNKNOWN',
      completion: terminalKnown ? row.requestFailedObserved ? 'SDK_FAILED' : 'SDK_FINISHED' : 'UNKNOWN',
      receiptStatus: terminalKnown && (row.requestFinishedObserved || row.responseObserved || row.errorTextCategory === 'SDK_CANCEL_REPORTED') ? 'SDK_TERMINAL_SOURCE_CONDITIONAL' : 'UNKNOWN',
      nativeStructuredReason: 'NOT_EXPOSED_BY_PUBLIC_API',
      nativeDocumentLoaderCancellation: 'UNKNOWN',
      nativeErrorObjectCorrespondence: 'UNKNOWN',
      nativeOrderingAgainstPageErrors: 'UNKNOWN',
      nativeCauseConclusive: false,
      productQualified: false,
    }
  }
  return {
    listeners: [['request', request], ['response', response], ['requestfailed', value => terminal(value, 'requestfailed')], ['requestfinished', value => terminal(value, 'requestfinished')]],
    guest, project,
    attached() { attached = true; closed = false },
    closed(value) { closed = value },
    privateRows() { return [...requests.values()].map(({ frame, origin, ...row }) => row) },
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
  const sdkSse = firstSseSdkCapture(page, boundScope, () => { failures++ })
  const acceptEvent = (event, ingress) => {
    try {
      if (!event || typeof event.document !== 'string' || !Number.isSafeInteger(event.sequence) || event.sequence <= 0) {
        failures++
        return
      }
      const received = structuredClone(event)
      sdkSse.guest(received)
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
  const onFailureSignal = (message) => {
    try {
      if (message.type() === 'error' && message.text() === '__c40_direct_observer_failure_v1__')
        failures++
    }
    catch { failures++ }
  }
  const listeners = [['pageerror', onError], ['console', onFailureSignal], ...sdkSse.listeners]
  let listenerAttached = false
  const detachListeners = () => {
    let detached = true
    for (const [kind, listener] of listeners) {
      try { page.off(kind, listener) }
      catch { failures++; detached = false }
    }
    listenerAttached = !detached
    sdkSse.closed(detached)
    return detached
  }
  return {
    async install() {
      if (listenerAttached)
        throw new Error('Direct owner observer installed twice')
      listenerAttached = true
      try {
        for (const [kind, listener] of listeners)
          page.on(kind, listener)
        sdkSse.attached()
        await closedTargetBound(() => page.exposeFunction('__c40RecordDirectOwner', (event) => {
          acceptEvent(event, performance.now())
        }), 'direct owner binding')
        await closedTargetBound(() => page.addInitScript(observeDirectOwners, boundScope), 'direct owner bootstrap')
      }
      catch (error) {
        if (!detachListeners())
          throw new AggregateError([error, new Error('Direct owner listener cleanup UNKNOWN')], 'Observer installation and cleanup failed', { cause: error })
        throw error
      }
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
      const events = [...journal.values()].map(value => value.event)
      return {
        privateEvidence: { journal: [...journal.values()].map(value => ({ ...value.event, hostIngress: value.hostIngress })), conflicts, pageErrors, observerFailures: failures, sdkSse: sdkSse.privateRows() },
        projection: { ...projectDirectOwners(events, pageErrors, failures, closure), sdkSse: sdkSse.project(events, pageErrors, closure, failures) },
      }
    },
    dispose() {
      if (!listenerAttached)
        return true
      return detachListeners()
    },
  }
}
