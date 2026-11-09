import assert from 'node:assert/strict'

export async function closedTargetBound(operation, label) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Closed-target observation deadline: ' + label)), 7000) }),
    ])
  }
  finally { clearTimeout(timer) }
}

// Register the acquisition before observing it. Cleanup owns both the returned
// handle and a late successful acquisition; a missed deadline never drops it.
export function ownSdkResource(create, close) {
  let closing = false
  let closeResult
  const closeOnce = value => closeResult ??= Promise.resolve().then(() => close(value))
  const result = Promise.resolve().then(create).then(async (value) => {
    if (closing)
      await closeOnce(value)
    return value
  })
  return {
    result,
    release() {
      closing = true
      return result.then(closeOnce)
    },
  }
}

export function closedTargetPrototype(reference, errors) {
  assert.equal(reference instanceof Error, true)
  assert.equal(reference instanceof errors.TimeoutError, false)
  const prototype = Object.getPrototypeOf(reference)
  const shared = Object.getPrototypeOf(errors.TimeoutError.prototype)
  assert.notEqual(prototype, Error.prototype)
  assert.notEqual(prototype, errors.TimeoutError.prototype)
  assert.notEqual(prototype, shared)
  assert.equal(Object.getPrototypeOf(prototype), shared)
  const constructor = Object.getOwnPropertyDescriptor(prototype, 'constructor')
  assert.equal(typeof constructor?.value, 'function')
  assert.equal(constructor.value.prototype, prototype)
  return prototype
}

export function isClosedTargetError(error, prototype) {
  return error instanceof Error && Object.getPrototypeOf(error) === prototype
}

// The reference belongs to this loaded SDK instance. Never serialize or reuse
// the prototype in another Node process; names/messages are diagnostics only.
export async function acquireClosedTargetReference(browser, errors, within = closedTargetBound) {
  let context, page, prototype, primaryFailure, contextOwner, pageOwner
  let pageClosed = false
  let contextClosed = false
  let crashed = false
  const pageErrors = []
  const failures = []
  let referenceType
  try {
    assert.equal(browser.isConnected(), true)
    contextOwner = ownSdkResource(() => browser.newContext(), async (value) => { await value.close(); contextClosed = true })
    context = await within(() => contextOwner.result, 'reference context')
    pageOwner = ownSdkResource(() => context.newPage(), async (value) => { await value.close(); pageClosed = true })
    page = await within(() => pageOwner.result, 'reference blank page')
    page.on('pageerror', error => pageErrors.push(error))
    page.on('crash', () => { crashed = true })
    assert.equal(page.url(), 'about:blank')
    await within(() => pageOwner.release(), 'reference page close')
    assert.equal(page.isClosed(), true)
    assert.equal(browser.isConnected(), true)
    assert.equal(browser.contexts().includes(context), true)
    assert.equal(context.pages().length, 0)

    const titleError = async () => {
      // Capture only the SDK rejection inside the bound. An outer deadline
      // cannot become the canonical reference.
      const result = await within(async () => {
        try { await page.title(); return {} }
        catch (error) { return { error } }
      }, 'closed page title')
      assert.equal(result.error instanceof Error, true)
      return result.error
    }
    const reference = await titleError()
    prototype = closedTargetPrototype(reference, errors)
    referenceType = reference.constructor.name
    assert.equal(isClosedTargetError(await titleError(), prototype), true)
    const closedGoto = await within(async () => {
      try { await page.goto('about:blank'); return {} }
      catch (error) { return { error } }
    }, 'closed page goto')
    assert.equal(isClosedTargetError(closedGoto.error, prototype), true)
    assert.equal(browser.isConnected(), true)
    assert.equal(browser.contexts().includes(context), true)
    assert.equal(crashed, false)
    assert.deepEqual(pageErrors, [])

    class TargetClosedError extends Error {}
    class TargetClosedError2 extends Error {}
    const shared = Object.getPrototypeOf(errors.TimeoutError.prototype)
    const CommonError = Object.getOwnPropertyDescriptor(shared, 'constructor').value
    for (const unrelated of [
      new Error('ordinary error'),
      new errors.TimeoutError('timeout'),
      new CommonError('shared SDK base'),
      new TargetClosedError('name lookalike'),
      new TargetClosedError2('bundled name lookalike'),
    ]) {
      assert.equal(isClosedTargetError(unrelated, prototype), false)
    }
  }
  catch (error) { primaryFailure = error }
  finally {
    const owners = [
      ...(pageOwner ? [['reference page', () => pageOwner.release()]] : []),
      ...(contextOwner ? [['reference context', () => contextOwner.release()]] : []),
    ]
    await Promise.all(owners.map(async ([actor, close]) => {
      try { await within(close, actor + ' cleanup') }
      catch (error) { failures.push(error) }
    }))
  }
  if (primaryFailure || failures.length)
    throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...failures], 'Closed-target SDK reference failed')
  assert.equal(pageClosed, true)
  assert.equal(contextClosed, true)
  return {
    prototype,
    receipt: { reference: 'owned closed about:blank page.title', referenceType, pageClosed, contextClosed, pageErrors: pageErrors.length, crashed, exactPrototype: true, closedGotoMatches: true, negativeControls: 5, cancellationEvidence: false },
  }
}

export async function qualifyClosedTargetSdk(browserType, errors, within = closedTargetBound) {
  let browser, reference, primaryFailure, browserOwner
  const failures = []
  let browserClosed = false
  try {
    browserOwner = ownSdkResource(() => browserType.launch({ timeout: 7000 }), async (value) => {
      await value.close()
      assert.equal(value.isConnected(), false)
      browserClosed = true
    })
    browser = await within(() => browserOwner.result, 'SDK browser launch')
    reference = await acquireClosedTargetReference(browser, errors, within)
  }
  catch (error) { primaryFailure = error }
  finally {
    if (browserOwner) {
      try {
        await within(() => browserOwner.release(), 'SDK browser close')
      }
      catch (error) { failures.push(error) }
    }
  }
  if (primaryFailure || failures.length)
    throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...failures], 'Closed-target SDK qualification failed')
  assert.equal(browserClosed, true)
  return { ...reference.receipt, browserClosed, sdkClassifierOnly: true, productQualified: false }
}
