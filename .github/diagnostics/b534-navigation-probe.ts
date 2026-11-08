
import { writeFileSync as saveB534Journal } from 'node:fs'
import process from 'node:process'

{
  const control = process.env.CAIRN_B534_CONTROL
  if (control !== 'native-cancel' && control !== 'abort-before-goto')
    throw new Error('CAIRN_B534_CONTROL must select one closed causal control')
  test(`B534 native Agents navigation (${control})`, async ({ page, workspace }, testInfo) => {
    const journal: Array<{ kind: string, hostAt: number, browserAt?: number, document?: string, poll?: number }> = []
    const errors: string[] = []
    let epoch = 0
    const documentEpochs = new Map<string, number>()
    const fetchCalls = new Map<string, { document: string, method: string, path: string, aborted: boolean }>()
    const fetchFailures: Array<{ kind: string, document?: string, poll?: number }> = []
    const nativeSourceErrors: Array<{ document?: string, source?: number }> = []
    const bodyFailures: Array<{ kind: string, document?: string, poll?: number }> = []
    const journalPath = testInfo.outputPath('b534-navigation.json')
    const saveJournal = () => saveB534Journal(journalPath, JSON.stringify({ control, journal, errors, bodyFailures, nativeSourceErrors, fetchFailures }))
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
      if (kind.endsWith('-body-rejected'))
        bodyFailures.push({ kind, document, poll })
      journal.push({ kind, hostAt: Date.now(), browserAt, document, poll })
      saveJournal()
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
      await expect.poll(() => page.evaluate(() => (window as any).shellPending.size), {
        message: 'Current document finite HTTP work and body continuations have settled',
      }).toBe(0)
      // This round trip follows the completed fetch microtasks and their native diagnostics.
      await page.evaluate(() => {})
      expect(bodyFailures).toEqual([])
    }

    function validateObservedOutcomes() {
      expect(errors).toEqual([])
      expect(bodyFailures).toEqual([])
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
      for (const status of streamStatuses.values()) {
        if (status !== 200)
          throw new Error(`An observed native chat stream has unexpected HTTP status ${status}`)
      }
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
      expect(nativeSourceErrors).toEqual([])
      if (deliveryFailure && (!targetPoll || requests.get(targetPoll) !== 'cancelled'))
        throw deliveryFailure
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
      if (routeFailure)
        throw routeFailure
    }

    record('setup-entered')
    page.on('pageerror', (error) => {
      errors.push(error.message)
      record('page-error')
    })
    try {
      await page.exposeFunction('b534Record', record)
      await page.addInitScript(() => {
        const documentId = crypto.randomUUID()
        let pollId = 0
        const controllers = new WeakMap<AbortSignal, AbortController>()
        const NativeAbortController = window.AbortController
        window.AbortController = class extends NativeAbortController {
          constructor() {
            super()
            controllers.set(this.signal, this)
          }
        }
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
          await probe(`${kind}-cycle-completed`, poll)
          if (kind.endsWith('-headers') && consuming.has(poll))
            return
          pending.delete(poll)
        }
        const probe = (kind: string, poll?: number, request?: { method: string, path: string }) => {
          return (window as any).b534Record(kind, performance.now(), documentId, poll, request) as Promise<void>
        }
        void probe('document-start')
        for (const kind of ['beforeunload', 'pagehide', 'pageshow']) {
          window.addEventListener(kind, (event) => {
            const persisted = 'persisted' in event && event.persisted === true
            void probe(`${kind}-${persisted ? 'persisted' : 'ordinary'}`)
          })
        }
        window.addEventListener('cairn-transport-change', () => {
          void probe('transport-change')
        })
        window.addEventListener('cairn-transport-observation', (event) => {
          const { route, path } = (event as CustomEvent).detail
          if (path !== '/chats/stream')
            return
          observation.chatAccepted = true
          if (route === 'direct')
            observation.directChatAccepted = true
          void probe(route === 'direct' ? 'chat-batch-direct-accepted' : 'chat-batch-relay-accepted')
        })
        const NativeEventSource = window.EventSource
        window.EventSource = class extends NativeEventSource {
          constructor(url: string | URL, options?: EventSourceInit) {
            super(url, options)
            const chat = new URL(String(url), location.href).pathname.endsWith('/api/chats/stream')
            if (!chat)
              return
            if (observation.armed)
              observation.armedSources.push(this)
            void probe('native-chat-source-created')
            this.addEventListener('open', () => { void probe('native-chat-source-open') })
            this.addEventListener('error', () => { void probe('native-chat-source-error') })
            const close = this.close.bind(this)
            this.close = () => {
              close()
              void probe(this.readyState === NativeEventSource.CLOSED ? 'native-chat-source-closed' : 'native-chat-source-close-unknown')
            }
          }
        }
        const fetch = window.fetch.bind(window)
        window.fetch = (...args) => {
          const input = args[0]
          const url = typeof input === 'string' || input instanceof URL ? String(input) : input.url
          const path = new URL(url, location.href).pathname
          if (!path.startsWith('/api/') && path !== '/web-build.json')
            return fetch(...args)
          const category = path === '/api/installations' ? 'poll' : path.endsWith('/api/chats') ? 'chat-read' : 'http'
          const poll = ++pollId
          if (category === 'poll' && observation.armed && !observation.targetController) {
            const signal = args[1]?.signal
            observation.targetController = signal ? controllers.get(signal) : undefined
            observation.targetPollId = poll
            void probe(observation.targetController ? 'poll-controller-identified' : 'poll-controller-unknown', poll)
          }
          pending.add(poll)
          probe(`${category}-start`, poll, { method: (args[1]?.method || (input instanceof Request ? input.method : 'GET')).toUpperCase(), path })
          args[1]?.signal?.addEventListener('abort', () => probe(`${category}-abort`, poll), { once: true })
          const settled = (kind: string) => {
            // A posted task follows the application's await/catch/finally microtasks.
            // Body consumers keep this cycle pending through their continuation.
            void probe(`${category}-${kind}`, poll)
            completions.port2.postMessage({ kind: `${category}-${kind}`, poll })
          }
          try {
            return fetch(...args).then((response) => {
              for (const method of ['json', 'text', 'arrayBuffer', 'blob', 'formData'] as const) {
                const consume = response[method].bind(response)
                ;(response as any)[method] = (...options: unknown[]) => {
                  consuming.add(poll)
                  pending.add(poll)
                  return Reflect.apply(consume, response, options).then((body: unknown) => {
                    settled('body-resolved')
                    return body
                  }, (cause: unknown) => {
                    settled('body-rejected')
                    throw cause
                  })
                }
              }
              settled('headers')
              return response
            }, (cause) => {
              settled(`rejected-${args[1]?.signal?.aborted ? 'signal-aborted' : 'signal-active'}`)
              throw cause
            })
          }
          catch (cause) {
            settled(`threw-${args[1]?.signal?.aborted ? 'signal-aborted' : 'signal-active'}`)
            throw cause
          }
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
    finally {
      releasePoll.resolve()
      releaseNavigation.resolve()
      record('cleanup-entered')
      let cleanupCompleted = false
      try {
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
        cleanupCompleted = true
      }
      finally {
        record(cleanupCompleted ? 'cleanup-completed' : 'cleanup-failed')
        try {
          await testInfo.attach('b534-navigation', { path: journalPath, contentType: 'application/json' })
        }
        finally {
          await provider?.close()
          record('provider-closed')
        }
      }
    }
    record('outcome-validation-entered')
    try {
      validateObservedOutcomes()
      record('outcome-validation-completed')
    }
    catch (cause) {
      record('outcome-validation-failed')
      throw cause
    }
  })
}
