
import { writeFileSync as saveB534Journal } from 'node:fs'
import process from 'node:process'

{
  const control = process.env.CAIRN_B534_CONTROL
  if (control !== 'native-cancel' && control !== 'abort-before-goto')
    throw new Error('CAIRN_B534_CONTROL must select one closed causal control')
  test(`B534 native Agents navigation (${control})`, async ({ page, workspace }, testInfo) => {
    const journal: Array<{ kind: string, hostAt: number, browserAt?: number, document?: string, poll?: number }> = []
    const errors: string[] = []
    let hostObserverFailures = 0
    let epoch = 0
    const documentEpochs = new Map<string, number>()
    const fetchCalls = new Map<string, { document: string, method: string, path: string, aborted: boolean }>()
    const fetchFailures: Array<{ kind: string, document?: string, poll?: number }> = []
    const nativeSourceErrors: Array<{ document?: string, source?: number }> = []
    const bodyFailures: Array<{ kind: string, document?: string, poll?: number }> = []
    const journalFailures: unknown[] = []
    const journalPath = testInfo.outputPath('b534-navigation.json')
    const saveJournal = () => saveB534Journal(journalPath, JSON.stringify({ control, journal, errors, bodyFailures, nativeSourceErrors, fetchFailures, hostObserverFailures }))
    const record = (kind: string, browserAt?: number, document?: string, poll?: number, request?: { method: string, path: string }) => {
      if (kind === 'document-start' && document)
        documentEpochs.set(document, epoch)
      const callKey = `${document}:${poll}`
      if (document && request && kind.endsWith('-start'))
        fetchCalls.set(callKey, { document, method: request.method, path: request.path, aborted: false })
      if (kind.endsWith('-abort') || kind.endsWith('-signal-aborted')) {
        const call = fetchCalls.get(callKey)
        if (call)
          call.aborted = true
      }
      if ( /-(?:rejected|threw)-signal-(?:active|aborted)$/.test(kind))
        fetchFailures.push({ kind, document, poll })
      if (kind === 'native-chat-source-error')
        nativeSourceErrors.push({ document, source: poll })
      if (kind.endsWith('-body-rejected') || kind.endsWith('-body-threw'))
        bodyFailures.push({ kind, document, poll })
      if (kind === 'observer-failed')
        errors.push('Archived HTTP observer failed')
      journal.push({ kind, hostAt: Date.now(), browserAt, document, poll })
      try {
        saveJournal()
      }
      catch (cause) {
        journalFailures.push(cause)
      }
    }
    let departingEpoch: number | undefined
    let departingDocument: string | undefined
    const epochs = new Map<import('@playwright/test').Request, number>()
    page.on('framenavigated', (frame) => {
      if (frame === page.mainFrame()) {
        epoch++
        record('document-committed')
      }
    })
    const requests = new Map<import('@playwright/test').Request, 'pending' | 'finished' | 'cancelled' | 'failed'>()
    const statuses = new Map<import('@playwright/test').Request, number>()
    const streamStatuses = new Map<import('@playwright/test').Request, number>()
    const nativeStreams = new Map<import('@playwright/test').Request, 'pending' | 'finished' | 'cancelled' | 'failed'>()
    const chatStreamRequest = (request: import('@playwright/test').Request) => request.resourceType() === 'eventsource' && new URL(request.url()).pathname.endsWith('/api/chats/stream')
    const finiteRequest = (request: import('@playwright/test').Request) => {
      const path = new URL(request.url()).pathname
      return ['fetch', 'xhr'].includes(request.resourceType()) && (path.startsWith('/api/') || path === '/web-build.json')
    }
    page.on('request', (request) => {
      if (chatStreamRequest(request)) {
        nativeStreams.set(request, 'pending')
        epochs.set(request, epoch)
        record('native-chat-source-request')
      }
      if (finiteRequest(request)) {
        requests.set(request, 'pending')
        epochs.set(request, epoch)
      }
    })
    page.on('response', (response) => {
      const request = response.request()
      if (chatStreamRequest(request)) {
        streamStatuses.set(request, response.status())
        record(`native-chat-source-http-status-${response.status()}`)
      }
      if (requests.has(request)) {
        statuses.set(request, response.status())
        record(`network-poll-status-${response.status()}`)
      }
    })
    page.on('requestfinished', (request) => {
      if (nativeStreams.has(request)) {
        nativeStreams.set(request, 'finished')
        record('native-chat-source-request-finished')
      }
      if (requests.has(request)) {
        requests.set(request, 'finished')
        record('network-poll-finished')
      }
    })
    page.on('requestfailed', (request) => {
      if (nativeStreams.has(request)) {
        const cancelled = request.failure()?.errorText === 'Load request cancelled'
        nativeStreams.set(request, cancelled ? 'cancelled' : 'failed')
        record(cancelled ? 'native-chat-source-request-cancelled' : 'native-chat-source-request-failed')
      }
      if (requests.has(request)) {
        const cancelled = request.failure()?.errorText === 'Load request cancelled'
        requests.set(request, cancelled ? 'cancelled' : 'failed')
        record(cancelled ? 'network-poll-cancelled' : 'network-poll-failed')
      }
    })
    const releasePoll = Promise.withResolvers<void>()
    const releaseNavigation = Promise.withResolvers<void>()
    let pollIntercepted = false
    let targetPoll: import('@playwright/test').Request | undefined
    let targetJavaScriptPoll: number | undefined
    const expectedTargetRejection = control === 'native-cancel' ? 'poll-rejected-signal-active' : 'poll-rejected-signal-aborted'
    let deliveryFailure: unknown
    let pollHeld = false
    let navigationHeld = false
    let routeFailure: unknown
    let navigationFailure: unknown
    let navigation: Promise<unknown> | undefined
    let currentStreamHandoff = false
    let currentDocument = ''
    let provider: Awaited<ReturnType<typeof mcpProvider>> | undefined
    const reached = async (kind: 'poll' | 'navigation') => {
      await expect.poll(() => {
        if (routeFailure)
          throw routeFailure
        if (navigationFailure)
          throw navigationFailure
        return kind === 'poll' ? pollHeld : navigationHeld
      }, { message: `Reach the ${kind} response boundary` }).toBe(true)
    }
    const browserSettled = async () => {
      assertArchiveObserver()
      await expect.poll(() => page.evaluate(() => (window as any).shellPending.size), {
        message: 'Current document finite HTTP work and body continuations have settled',
      }).toBe(0)
      // This round trip follows the completed fetch microtasks and their native diagnostics.
      expect(await page.evaluate(() => (window as any).shellObserverFailed)).toBe(false)
      expect(bodyFailures).toEqual([])
    }

    function validateObservedOutcomes() {
      const auditFailures: unknown[] = []
      const audit = (run: () => void) => {
        try {
          run()
        }
        catch (cause) {
          auditFailures.push(cause)
        }
      }
      audit(() => {
        expect(errors).toEqual([])
      })
      audit(() => {
        expect(bodyFailures).toEqual([])
      })
      audit(() => {
        for (const [request, terminal] of requests) {
          if (terminal === 'pending')
            throw new Error('A finite HTTP request has no terminal receipt at the final boundary')
          if (request === targetPoll)
            record(`target-network-${terminal}`)
          const status = statuses.get(request)
          const successfulResponse = status !== undefined && status >= 200 && status < 300
          if ((status !== undefined && !successfulResponse) || (terminal === 'finished' && status === undefined))
            throw new Error(`An observed finite HTTP response has unexpected status ${status ?? 'unknown'}`)
          if (terminal === 'failed')
            throw new Error('An observed finite HTTP request failed unexpectedly')
          const requestEpoch = epochs.get(request)
          const retired = requestEpoch !== undefined && requestEpoch < epoch
          const read = ['GET', 'HEAD'].includes(request.method())
          const path = new URL(request.url()).pathname
          const directControl = request.method() === 'POST' && /^\/api\/installations\/[^/]+\/direct\/(?:authorize|[^/]+\/(?:signal|renew))$/.test(path)
          const abortedControl = directControl && [...fetchCalls.values()].some(call => call.path === path && call.method === 'POST'
            && call.aborted && documentEpochs.get(call.document) === requestEpoch
            && journal.some(event => event.document === call.document && event.kind.startsWith('pagehide-')))
          const withdrawn = retired && (read || abortedControl)
          if (terminal === 'cancelled' && request !== targetPoll && !withdrawn)
            throw new Error('An HTTP cancellation lacks old-document read provenance')
        }
      })
      audit(() => {
        for (const status of streamStatuses.values()) {
          if (status !== 200)
            throw new Error(`An observed native chat stream has unexpected HTTP status ${status}`)
        }
      })
      audit(() => {
        for (const [request, terminal] of nativeStreams) {
          const status = streamStatuses.get(request)
          if (terminal === 'pending' && epochs.get(request) !== epoch)
            throw new Error('A retired native stream has no terminal receipt at the final boundary')
          if (terminal === 'failed')
            throw new Error('A native chat request failed without an attributable cancellation')
          if ((terminal === 'finished' || terminal === 'pending') && status !== 200)
            throw new Error('A native chat request has no successful opening proof')
          if (terminal === 'cancelled') {
            const requestEpoch = epochs.get(request)
            const retired = requestEpoch !== undefined && requestEpoch < epoch
            const handoff = requestEpoch === epoch && status === 200 && currentStreamHandoff
            if (!retired && !handoff)
              throw new Error('A native chat cancellation lacks document retirement or successful direct handoff')
          }
        }
      })
      audit(() => {
        expect(nativeSourceErrors).toEqual([])
      })
      audit(() => {
        if (deliveryFailure && (!targetPoll || requests.get(targetPoll) !== 'cancelled'))
          throw deliveryFailure
      })
      audit(() => {
        for (const failure of fetchFailures) {
          const target = failure.document === departingDocument && failure.poll === targetJavaScriptPoll
          if (target) {
            if (!targetPoll || requests.get(targetPoll) !== 'cancelled')
              throw new Error('The target JavaScript rejection has no matching native cancellation')
            if (failure.kind !== expectedTargetRejection)
              throw new Error('The target rejection does not match its control-specific caller signal state')
            continue
          }
          const withdrawnCaller = failure.document !== currentDocument && failure.kind.endsWith('-signal-aborted')
            && journal.some(event => event.document === failure.document && event.kind.startsWith('pagehide-'))
          if (!withdrawnCaller)
            throw new Error('A JavaScript fetch rejection lacks target or retired-caller cancellation provenance')
        }
      })
      audit(() => {
        if (routeFailure)
          throw routeFailure
      })
      if (auditFailures.length)
        throw new AggregateError(auditFailures, 'B534 independent outcome audits failed')
    }

    const outcomeFailures: unknown[] = []
    const observedCleanup = async (operation: () => Promise<unknown>, label: string) => {
      let timer: ReturnType<typeof setTimeout> | undefined
      try {
        return await Promise.race([
          Promise.resolve().then(operation),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new Error(`B534 cleanup deadline: ${label}`)), 7000)
          }),
        ])
      }
      finally {
        clearTimeout(timer)
      }
    }

    record('setup-entered')
    const archiveFailureListener = (message: import('@playwright/test').ConsoleMessage) => {
      if (message.type() === 'error' && message.text() === '__c40_archived_observer_failure_v1__')
        hostObserverFailures++
    }
    const assertArchiveObserver = () => {
      if (hostObserverFailures > 0)
        throw new Error('Archived observation failure makes evidence UNKNOWN')
    }
    page.on('console', archiveFailureListener)
    page.once('close', () => page.off('console', archiveFailureListener))
    page.on('pageerror', (error) => {
      errors.push(error.message)
      record('page-error')
    })
    try {
      if (journalFailures.length)
        throw journalFailures[0]
      await page.exposeFunction('b534Record', record)
      await page.addInitScript(() => {
        'use strict'
        const documentId = crypto.randomUUID()
        let pollId = 0
        const controllers = new WeakMap<AbortSignal, AbortController>()
        const NativeAbortController = window.AbortController
        const observation = {
          documentId,
          armed: false,
          directChatAccepted: false,
          chatAccepted: false,
          armedSources: [] as EventSource[],
          targetController: undefined as AbortController | undefined,
          targetPollId: undefined as number | undefined,
        }
        ;(window as any).b534Observation = observation
        const pending = new Set<number>()
        const consuming = new Set<number>()
        ;(window as any).shellPending = pending
        const completions = new MessageChannel()
        completions.port1.onmessage = async (event) => {
          const { kind, poll } = event.data as { kind: string, poll: number }
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          try { await probe(`${kind}-cycle-completed`, poll) }
          catch { observerFailed(); return }
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          pending.delete(poll)
        }
        const probe = (kind: string, poll?: number, request?: { method: string, path: string }) => {
          return (window as any).b534Record(kind, performance.now(), documentId, poll, request) as Promise<void>
        }
        ;(window as any).shellObserverFailed = false
        const failureConsole = window.console
        const failureConsoleError = failureConsole.error
        const observerFailed = () => {
          if ((window as any).shellObserverFailed)
            return
          ;(window as any).shellObserverFailed = true
          try { Reflect.apply(failureConsoleError, failureConsole, ['__c40_archived_observer_failure_v1__']) } catch {}
          try { void probe('observer-failed').catch(() => {}) } catch {}
        }
        const safeProbe = (...args: Parameters<typeof probe>) => {
          try { void probe(...args).catch(observerFailed) } catch { observerFailed() }
        }
        safeProbe('document-start')
        for (const kind of ['beforeunload', 'pagehide', 'pageshow']) {
          window.addEventListener(kind, (event) => {
            const persisted = 'persisted' in event && event.persisted === true
            safeProbe(`${kind}-${persisted ? 'persisted' : 'ordinary'}`)
          })
        }
        window.addEventListener('cairn-transport-change', () => {
          safeProbe('transport-change')
        })
        window.addEventListener('cairn-transport-observation', (event) => {
          const { route, path } = (event as CustomEvent).detail
          if (path !== '/chats/stream')
            return
          observation.chatAccepted = true
          if (route === 'direct')
            observation.directChatAccepted = true
          safeProbe(route === 'direct' ? 'chat-batch-direct-accepted' : 'chat-batch-relay-accepted')
        })
        const NativeEventSource = window.EventSource
        const observedChatSources = new WeakSet<EventSource>()
        const nativeSourceClose = NativeEventSource.prototype.close
        NativeEventSource.prototype.close = function (this: EventSource, ...args: Parameters<typeof nativeSourceClose>) {
          const original = Reflect.apply(nativeSourceClose, this, args)
          try {
            if (observedChatSources.has(this))
              safeProbe(this.readyState === NativeEventSource.CLOSED ? 'native-chat-source-closed' : 'native-chat-source-close-unknown')
          }
          catch { observerFailed() }
          return original
        }
        window.EventSource = new Proxy(NativeEventSource, {
          construct(target, args, newTarget) {
            const original = Reflect.construct(target, args, newTarget) as EventSource
            try {
              if (typeof args[0] !== 'string') {
                observerFailed()
                return original
              }
              const chat = new URL(args[0], location.href).pathname.endsWith('/api/chats/stream')
              if (!chat)
                return original
              observedChatSources.add(original)
              if (observation.armed)
                observation.armedSources.push(original)
              safeProbe('native-chat-source-created')
              original.addEventListener('open', () => { safeProbe('native-chat-source-open') })
              original.addEventListener('error', () => { safeProbe('native-chat-source-error') })
            }
            catch { observerFailed() }
            return original
          },
        })
        const fetch = window.fetch
        const promiseThen = Promise.prototype.then
        const responses = new WeakMap<Response, { poll: number, settled: (kind: string) => void }>()
        const nativeControllerSignal = Object.getOwnPropertyDescriptor(NativeAbortController.prototype, 'signal')?.get
        window.AbortController = new Proxy(NativeAbortController, {
          construct(target, args, newTarget) {
            const original = Reflect.construct(target, args, newTarget) as AbortController
            try {
              if (nativeControllerSignal)
                controllers.set(Reflect.apply(nativeControllerSignal, original, []), original)
              else
                observerFailed()
            }
            catch { observerFailed() }
            return original
          },
        })
        const observe = <T>(original: Promise<T>, fulfilled: (value: T) => void, rejected: () => void) => {
          try {
            const observation = Reflect.apply(promiseThen, original, [
              (value: T) => { try { fulfilled(value) } catch { observerFailed() } },
              () => { try { rejected() } catch { observerFailed() } },
            ])
            void Reflect.apply(promiseThen, observation, [undefined, observerFailed])
          }
          catch { observerFailed() }
        }
        for (const method of ['json', 'text', 'arrayBuffer', 'blob', 'formData'] as const) {
          const consume = Response.prototype[method]
          if (typeof consume !== 'function')
            continue
          ;(Response.prototype as any)[method] = function (this: Response, ...options: unknown[]) {
            const metadata = responses.get(this)
            if (!metadata)
              return Reflect.apply(consume, this, options)
            consuming.add(metadata.poll)
            pending.add(metadata.poll)
            let original
            try {
              original = Reflect.apply(consume, this, options)
            }
            catch (cause) {
              metadata.settled('body-threw')
              throw cause
            }
            observe(original, () => metadata.settled('body-resolved'), () => metadata.settled('body-rejected'))
            return original
          }
        }
        const ordinaryData = (value: unknown): Record<string, unknown> | null => {
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
              const descriptor = Object.getOwnPropertyDescriptor(inherited, key)!.value as PropertyDescriptor
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
            const descriptor = Object.getOwnPropertyDescriptor(descriptors, key)!.value as PropertyDescriptor
            if (!Object.hasOwn(descriptor, 'value'))
              return null
            data[key] = descriptor.value
          }
          return data
        }
        const requestData = (value: unknown): Record<string, unknown> | null => {
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

        window.fetch = function (this: Window, ...args: Parameters<typeof fetch>) {
          if (this != null && this !== window)
            return Reflect.apply(fetch, this, args)
          let path: string | undefined
          let signal: AbortSignal | undefined
          let method = 'GET'
          let metadataKnown = true
          try {
            if (typeof args[0] !== 'string') {
              metadataKnown = false
            }
            else {
              path = new URL(args[0], location.href).pathname
              if (path.startsWith('/api/') || path === '/web-build.json') {
                const data = requestData(args[1])
                if (!data || (data.method !== undefined && typeof data.method !== 'string')) {
                  metadataKnown = false
                }
                else {
                  method = (data.method as string | undefined ?? 'GET').toUpperCase()
                  signal = data.signal as AbortSignal | undefined
                  if (signal !== undefined && !(signal instanceof AbortSignal))
                    metadataKnown = false
                }
              }
            }
          }
          catch { metadataKnown = false }
          let original: ReturnType<typeof fetch>
          try {
            original = Reflect.apply(fetch, this, args)
          }
          catch (cause) {
            observerFailed()
            throw cause
          }
          if (!metadataKnown) {
            observerFailed()
            return original
          }
          if (!path || (!path.startsWith('/api/') && path !== '/web-build.json'))
            return original
          try {
            const category = path === '/api/installations' ? 'poll' : path.endsWith('/api/chats') ? 'chat-read' : 'http'
            const poll = ++pollId
            if (category === 'poll' && observation.armed && !observation.targetController) {
              observation.targetController = signal ? controllers.get(signal) : undefined
              observation.targetPollId = poll
              safeProbe(observation.targetController ? 'poll-controller-identified' : 'poll-controller-unknown', poll)
            }
            pending.add(poll)
            safeProbe(`${category}-start`, poll, { method, path })
            signal?.addEventListener('abort', () => safeProbe(`${category}-abort`, poll), { once: true })
            const settled = (kind: string) => {
              // Separate observation returns the ORIGINAL promise. MessagePort
              // completion still follows the application's native microtasks.
              safeProbe(`${category}-${kind}`, poll)
              completions.port2.postMessage({ kind: `${category}-${kind}`, poll })
            }
            observe(original, (response) => {
              responses.set(response, { poll, settled })
              settled('headers')
            }, () => settled(`rejected-${signal?.aborted ? 'signal-aborted' : 'signal-active'}`))
          }
          catch { observerFailed() }
          return original
        }
      })
      await page.setViewportSize({ width: 390, height: 844 })
      await page.emulateMedia({ colorScheme: 'dark' })
      await page.clock.install()
      provider = await mcpProvider()
      await workspace.api('/api/mcps', 'POST', {
        name: 'B534 departure', transport: 'http', url: `${provider.origin}/mcp`, auth: 'oauth', allowPrivateNetwork: true,
      })
      await page.goto(workspacePath('/mcps'))
      const accountView = page.getByLabel('Email address').or(page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true }))
      await expect(accountView.first()).toBeAttached()
      await browserSettled()
      await authenticateWorkspace(page)
      await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
      await expect.poll(() => page.evaluate(() => (window as any).b534Observation.directChatAccepted), {
        message: 'The pre-OAuth shell accepted real direct chat-list traffic',
      }).toBe(true)
      await browserSettled()
      const card = page.locator('.mcp-card').filter({ hasText: 'B534 departure' })
      await card.getByRole('button', { name: 'Connect', exact: true }).click()
      await page.waitForURL(url => url.pathname.endsWith('/mcps/callback'))
      await expect(card.getByText('Connected', { exact: true })).toBeVisible()
      expect(provider.exchanges).toBe(1)
      await expect.poll(() => page.evaluate(() => (window as any).b534Observation.directChatAccepted), {
        message: 'The real shell accepted a conversation-list batch over direct transport',
      }).toBe(true)
      await expect(page.locator('[data-transport-route]').first()).toHaveAttribute('data-transport-route', 'direct')
      await page.clock.pauseAt(new Date(Date.now() + 1000))
      await browserSettled()
      departingDocument = await page.evaluate(() => {
        const observation = (window as any).b534Observation
        observation.armed = true
        return observation.documentId as string
      })
      await page.route('**/api/installations', async (route) => {
        // Hold exactly the old document's targeted request; the new document stays real.
        if (pollIntercepted)
          return route.continue()
        pollIntercepted = true
        targetPoll = route.request()
        try {
          const response = await route.fetch({ timeout: 7000 })
          record(`poll-response-status-${response.status()}`)
          expect(response.status()).toBe(200)
          pollHeld = true
          record('poll-response-held')
          await releasePoll.promise
          try {
            await route.fulfill({ response })
            record('poll-response-delivered')
          }
          catch (cause) {
            deliveryFailure = cause
            record('poll-response-not-delivered')
          }
        }
        catch (cause) {
          routeFailure = cause
          record('poll-route-failed')
          await route.abort().catch(() => {})
        }
      })
      await page.route('**/agents', async (route) => {
        if (!route.request().isNavigationRequest())
          return route.continue()
        try {
          const response = await route.fetch({ timeout: 7000 })
          record(`navigation-response-status-${response.status()}`)
          expect(response.status()).toBe(200)
          navigationHeld = true
          record('navigation-response-held')
          await releaseNavigation.promise
          await route.fulfill({ response })
          record('navigation-response-delivered')
        }
        catch (cause) {
          routeFailure = cause
          record('navigation-route-failed')
          await route.abort().catch(() => {})
        }
      })
      await page.clock.runFor(3100)
      await reached('poll')
      await expect.poll(() => [...requests].every(([request, state]) => request === targetPoll || state !== 'pending'), {
        message: 'Other observed finite HTTP has terminated before departure',
      }).toBe(true)
      await expect.poll(() => page.evaluate(() => (window as any).shellPending.size)).toBe(1)
      await expect(page.locator('[data-transport-route]').first()).toHaveAttribute('data-transport-route', 'direct')
      const identified = await page.evaluate(() => {
        const observation = (window as any).b534Observation
        const controller = observation.targetController as AbortController | undefined
        return { active: !!controller && !controller.signal.aborted, poll: observation.targetPollId, document: observation.documentId }
      })
      expect(identified.active).toBe(true)
      expect(identified.document).toBe(departingDocument)
      expect(typeof identified.poll).toBe('number')
      targetJavaScriptPoll = identified.poll
      if (control === 'abort-before-goto') {
        const aborted = await page.evaluate(() => {
          const controller = (window as any).b534Observation.targetController as AbortController
          controller.abort()
          return controller.signal.aborted
        })
        expect(aborted).toBe(true)
        await browserSettled()
        await expect(page.locator('[data-transport-route]').first()).toHaveAttribute('data-transport-route', 'direct')
        record('control-poll-aborted-before-goto')
      }
      departingEpoch = epoch
      record('agents-goto-entered')
      navigation = page.goto(workspacePath('/agents'))
      void navigation.catch((cause) => {
        navigationFailure = cause
        record('agents-goto-failed')
      })
      await reached('navigation')
      await expect.poll(() => {
        if (routeFailure)
          throw routeFailure
        return fetchFailures.some(failure => failure.document === departingDocument && failure.poll === targetJavaScriptPoll && failure.kind === expectedTargetRejection)
      }, { message: 'The actual old-document poll has rejected or been explicitly aborted' }).toBe(true)
      record('old-document-poll-rejection-observed')
      await page.evaluate(() => new Promise<void>((resolve) => {
        const barrier = new MessageChannel()
        barrier.port1.onmessage = () => {
          barrier.port1.close()
          barrier.port2.close()
          resolve()
        }
        barrier.port2.postMessage(null)
      }))
      await expect.poll(() => page.evaluate(() => {
        const sources = (window as any).b534Observation.armedSources as EventSource[]
        return sources.every(source => source.readyState !== EventSource.CONNECTING)
      }), { message: 'Every native chat source admitted by the old document has opened or closed' }).toBe(true)
      record('old-document-native-source-outcomes-observed')
      await browserSettled()
      expect(await page.evaluate(() => (window as any).b534Observation.documentId)).toBe(departingDocument)
      record('old-document-finite-javascript-settled')
      releaseNavigation.resolve()
      await navigation
      releasePoll.resolve()
      await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
      await expect.poll(() => page.evaluate(() => (window as any).b534Observation.chatAccepted), {
        message: 'The new document has accepted its first real shell conversation-list batch',
      }).toBe(true)

    }
    catch (cause) {
      outcomeFailures.push(cause)
      record('workload-failed')
    }
    finally {
      releasePoll.resolve()
      releaseNavigation.resolve()
      record('cleanup-entered')
      let cleanupCompleted = false
      try {
        await observedCleanup(async () => {
          await navigation?.catch(() => {})
          await page.unrouteAll({ behavior: 'wait' })
          await expect.poll(() => [...requests.values()].every(state => state !== 'pending'), {
            message: 'Every observed finite application HTTP request has terminated',
          }).toBe(true)
          await expect.poll(() => [...nativeStreams].every(([request, terminal]) => epochs.get(request) === epoch || terminal !== 'pending'), {
            message: 'Every retired-document native chat request has terminated',
          }).toBe(true)
          currentStreamHandoff = await page.evaluate(() => {
            const observation = (window as any).b534Observation
            return observation.directChatAccepted
              && document.querySelector('[data-transport-route]')?.getAttribute('data-transport-route') === 'direct'
          })
          await browserSettled()
          currentDocument = await page.evaluate(() => (window as any).b534Observation.documentId as string)
        }, 'document observations')
        cleanupCompleted = true
      }
      catch (cause) {
        outcomeFailures.push(cause)
      }
      finally {
        record(cleanupCompleted ? 'cleanup-completed' : 'cleanup-failed')
        const owners = await Promise.allSettled([
          observedCleanup(() => page.close(), 'page retirement'),
          observedCleanup(() => testInfo.attach('b534-navigation', { path: journalPath, contentType: 'application/json' }), 'attachment'),
          observedCleanup(async () => {
            await provider?.close()
            record('provider-closed')
          }, 'provider'),
        ])
        for (const owner of owners) {
          if (owner.status === 'rejected')
            outcomeFailures.push(owner.reason)
        }
      }
    }
    try {
      await observedCleanup(async () => {
        expect(page.isClosed()).toBe(true)
        await expect.poll(() => [...requests.values()].every(state => state !== 'pending'), {
          message: 'All finite HTTP requests terminate after page retirement',
        }).toBe(true)
        await expect.poll(() => [...nativeStreams.values()].every(state => state !== 'pending'), {
          message: 'All native stream requests terminate after page retirement',
        }).toBe(true)
      }, 'post-retirement terminal receipts')
    }
    catch (cause) {
      outcomeFailures.push(cause)
    }
    record('outcome-validation-entered')
    try {
      validateObservedOutcomes()
      record('outcome-validation-completed')
    }
    catch (cause) {
      record('outcome-validation-failed')
      outcomeFailures.push(cause)
    }
    try { assertArchiveObserver() } catch (cause) { outcomeFailures.push(cause) }
    if (outcomeFailures.length || journalFailures.length)
      throw new AggregateError([...outcomeFailures, ...journalFailures], 'B534 workload, cleanup, journal or outcome validation failed')
  })
}
