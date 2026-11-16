import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { createNativeAndroidDevice } from './native-android-device'
import {
  createNativeTargetTracker,
  nativeInputFocused,
  nativeNodes,
  nativePositionIn as positionIn,
} from './native-android-ui'
import { rulesFixture } from './rules-fixture'

// Opt-in journey against the real installation and Beacon. Use a disposable emulator:
// ANDROID_SERIAL=emulator-5580 CAIRN_RULES_ANDROID=1 pnpm test:e2e --project=rules-android
// Build/install the debug APK with CAIRN_BEACON_ORIGIN=http://localhost:4422 first.
// CAIRN_RULES_ANDROID_RESET=1 clears only this emulator's Cairn app before sign-in.
const serial = process.env.ANDROID_SERIAL
const binary = path.join(process.env.ANDROID_HOME ?? '', 'platform-tools/adb')
const device = createNativeAndroidDevice(binary, serial)
const adb = device.command
let diagnosticTap = 0
const originalAnimations = new Map<string, string>()
const animationSettings = ['window_animation_scale', 'transition_animation_scale', 'animator_duration_scale']

test.beforeAll(async () => {
  if (process.env.CAIRN_RULES_ANDROID !== '1')
    return
  expect((await adb('shell', 'getprop', 'ro.kernel.qemu')).trim()).toBe('1')
  for (const setting of animationSettings)
    originalAnimations.set(setting, (await adb('shell', 'settings', 'get', 'global', setting)).trim())
  for (const setting of animationSettings)
    await adb('shell', 'settings', 'put', 'global', setting, '0')
})

test.afterAll(async () => {
  for (const [setting, value] of originalAnimations) {
    if (value === 'null')
      await adb('shell', 'settings', 'delete', 'global', setting)
    else
      await adb('shell', 'settings', 'put', 'global', setting, value)
  }

  originalAnimations.clear()
})

async function rawScreen() {
  await adb('shell', 'uiautomator', 'dump', '/sdcard/cairn-rules-journey.xml')
  return await adb('exec-out', 'cat', '/sdcard/cairn-rules-journey.xml')
}

async function screen() {
  let document = await rawScreen()
  if (document.includes('Mise à jour disponible')) {
    const postpone = positionIn(document, 'Plus tard')
    if (!postpone)
      throw new Error('The update prompt did not expose its postpone button.')
    await tapPosition(postpone)
    await expect.poll(async () => {
      document = await rawScreen()
      return !document.includes('Mise à jour disponible')
    }, { timeout: 10000 }).toBe(true)
  }

  return document
}

async function tapPosition(position: number[]) {
  await adb('shell', 'input', 'tap', String(position[0]), String(position[1]))
}

async function tap(text: string, preferEditable = false) {
  const ready = createNativeTargetTracker()
  await expect.poll(async () => {
    const current = await screen()
    const position = ready(current, text, preferEditable)
    if (!position)
      return false

    const diagnostic = ['Modifier', 'Terminé · 1 actions'].includes(text) ? ++diagnosticTap : 0
    if (diagnostic) {
      await writeFile(test.info().outputPath(`tap-${diagnostic}-before.xml`), current)
      await writeFile(test.info().outputPath(`tap-${diagnostic}.json`), JSON.stringify({ text, position }))
    }

    await tapPosition(position)
    if (diagnostic)
      await writeFile(test.info().outputPath(`tap-${diagnostic}-after.xml`), await screen())
    return true
  }, { timeout: 25000, intervals: [500] }).toBe(true)
}

async function scrollTo(text: string, preferEditable = false) {
  for (let attempt = 0; attempt < 7; attempt++) {
    const current = await screen()
    if (positionIn(current, text, false, preferEditable)) {
      await tap(text, preferEditable)
      return
    }

    const clipped = positionIn(current, text, true, preferEditable)
    if (clipped && clipped[1] < 400)
      await adb('shell', 'input', 'swipe', '360', '500', '360', '700', '350')
    else
      await adb('shell', 'input', 'swipe', '360', '900', '360', '450', '450')
  }

  throw new Error(`The native screen did not expose ${text}.`)
}

async function fill(label: string, value: string | (() => string)) {
  await scrollTo(label, true)
  await expect.poll(async () => nativeInputFocused(await screen(), label), { timeout: 7000 }).toBe(true)
  const text = typeof value === 'function' ? value() : value
  await adb('shell', 'input', 'keycombination', 'KEYCODE_CTRL_LEFT', 'KEYCODE_A')
  await adb('shell', 'input', 'keyevent', 'KEYCODE_DEL')
  await expect.poll(async () => nativeNodes(await screen()).some(node => node.attributes.class === 'android.widget.EditText' && node.attributes.focused === 'true' && node.attributes.text === ''), { timeout: 7000 }).toBe(true)
  const shellText = text.replaceAll(' ', '%s').replaceAll('\'', '\'\\\'\'')
  await adb('shell', 'input', 'text', `'${shellText}'`)
  await expect.poll(async () => nativeNodes(await screen()).some(node => node.attributes.class === 'android.widget.EditText' && node.attributes.focused === 'true' && node.attributes.text === text), { timeout: 7000 }).toBe(true)
  await adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
}

async function finishStep() {
  const document = await screen()
  if (document.includes('content-desc="Close sheet"')) {
    await scrollTo('Terminer cette étape')
    return
  }

  // Back may already dismiss the sheet when the keyboard was not visible.
  expect(document).toContain('text="Enregistrer"')
}

async function capture(file: string) {
  await writeFile(file, await device.screenshot())
}

test('native rules persist edits, integer waits and every creator control against the real service', async ({ page, workspace }, info) => {
  // Includes the real email cooldown and each verified native edit/control/history step.
  // Individual assertions retain their own bounds.
  test.setTimeout(480000)
  expect((await adb('shell', 'getprop', 'ro.kernel.qemu')).trim()).toBe('1')
  expect(await adb('shell', 'wm', 'size')).toContain('720x1280')
  await page.setViewportSize({ width: 1440, height: 1100 })
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: { ...fixture.definition, limits: { triggeringsPerHour: 1, conversationsPerHour: 5 } },
    enabled: true,
  })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'A real local rule execution' }, 'native-first')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await signIn(page)
  const emailSentAt = Date.now()
  await page.goto(workspacePath('/rules'))
  await expect(page.getByRole('region', { name: 'Rule list', exact: true }).getByText('Support messages', { exact: true })).toBeVisible()
  await page.screenshot({ path: info.outputPath('web-list.png'), animations: 'disabled' })
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await expect(page.getByRole('heading', { name: 'Support messages', exact: true })).toBeVisible()
  await page.screenshot({ path: info.outputPath('web-detail.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await page.getByRole('button', { name: /Completed.*1 action/ }).click()
  await page.getByText('Result', { exact: true }).click()
  await expect(page.locator('details[open] pre').filter({ hasText: 'Received: A real local rule execution' })).toBeVisible()
  await page.screenshot({ path: info.outputPath('web-history.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Flow', exact: true }).click()
  await page.getByRole('button', { name: 'Edit', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Save version', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true }).click()
  await expect(page.getByText('#support', { exact: true }).first()).toBeVisible()
  await page.getByRole('textbox', { name: 'Rule name', exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: info.outputPath('web-editor.png'), animations: 'disabled', fullPage: true })
  const port = new URL(workspace.url).port
  await adb('reverse', `tcp:${port}`, `tcp:${port}`)
  await adb('shell', 'am', 'force-stop', 'build.cairn.app')
  if (process.env.CAIRN_RULES_ANDROID_RESET === '1')
    await adb('shell', 'pm', 'clear', 'build.cairn.app')
  await adb('shell', 'am', 'start', '-n', 'build.cairn.app/.MainActivity')
  await fill('Adresse e-mail', workspace.emailProof().email)
  // Keep Beacon's actual per-address email delivery cooldown.
  await expect.poll(() => Date.now() - emailSentAt, { timeout: 70000, intervals: [2000] }).toBeGreaterThan(62000)
  await tap('Recevoir un code')
  await fill('Code reçu par e-mail', () => workspace.emailProof().code)
  await tap('Se connecter')
  await tap('Atelier')
  await tap('Règles')
  await expect.poll(async () => (await screen()).includes('Support messages')).toBe(true)
  await capture(info.outputPath('android-list.png'))
  await tap('Support messages')
  await expect.poll(async () => Boolean(positionIn(await screen(), 'Modifier'))).toBe(true)
  await capture(info.outputPath('android-detail.png'))
  await tap('Modifier')
  await fill('Nom de la règle', 'Support messages mobile')
  await expect.poll(async () => Boolean(positionIn(await screen(), 'Enregistrer'))).toBe(true)
  await capture(info.outputPath('android-editor.png'))
  await adb('shell', 'input', 'swipe', '360', '1000', '360', '300', '600')
  await capture(info.outputPath('android-flow.png'))
  await adb('shell', 'input', 'swipe', '360', '1000', '360', '300', '600')
  await capture(info.outputPath('android-flow-lower.png'))
  await workspace.api(`/api/hook-rules/${rule.id}/enabled`, 'POST', { ruleId: rule.id, enabled: false })
  await workspace.api(`/api/hook-rules/${rule.id}/enabled`, 'POST', { ruleId: rule.id, enabled: true })
  await tap('Enregistrer')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${rule.id}`)).definition.name).toBe('Support messages mobile')
  await scrollTo('Désactiver')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks.map((block: any) => block.kind)).toContain('disabled')
  await scrollTo('Réactiver')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks).toEqual([])
  await scrollTo('Pause d’urgence')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks.map((block: any) => block.kind)).toContain('emergencyPaused')
  await scrollTo('Lever la pause')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks).toEqual([])
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'Exhaust this actual hourly budget' }, 'native-second')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks.map((block: any) => block.kind), { timeout: 20000 }).toContain('budgetExhausted')
  const exhaustedRevision = (await workspace.api(`/api/hook-rules/${rule.id}`)).revision
  // Reopen the detail at its start after the asynchronous budget change.
  await adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await tap('Support messages mobile')
  await scrollTo('Relancer le budget')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).revision).not.toBe(exhaustedRevision)
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks).toEqual([])
  await scrollTo('Dupliquer')
  await expect.poll(async () => (await workspace.api('/api/hook-rules')).rules.length).toBe(2)
  const copy = (await workspace.api('/api/hook-rules')).rules.find((item: any) => item.id !== rule.id)
  expect(copy.blocks.map((block: any) => block.kind)).toContain('disabled')
  await expect.poll(async () => (await screen()).includes(copy.name)).toBe(true)
  await scrollTo('Supprimer')
  await tap('Supprimer')
  await expect.poll(async () => (await workspace.api('/api/hook-rules')).rules.map((item: any) => item.id)).toEqual([rule.id])
  const waiting = await workspace.api('/api/hook-rules', 'POST', {
    definition: {
      ...fixture.definition,
      name: 'Native wait',
      steps: [
        { type: 'action', id: 'pause', action: { type: 'wait', seconds: { type: 'literal', value: 600 } } },
        {
          type: 'action',
          id: 'remember',
          label: 'Syntaxe JSON',
          action: {
            type: 'storeValue',
            scope: 'rule',
            name: 'last',
            value: { type: 'literal', value: 'done' },
          },
        },
      ],
    },
    enabled: false,
  })
  await scrollTo('Native wait')
  await scrollTo('Modifier')
  await scrollTo('Attendre 10 min')
  await scrollTo('600')
  await fill('600', '900')
  for (let step = 0; step < 2 && !(await screen()).includes('Enregistrer'); step++)
    await adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await tap('Enregistrer')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${waiting.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${waiting.id}`)).definition.steps[0].action.seconds).toEqual({ type: 'literal', value: 900 })
  await scrollTo('Modifier')
  await scrollTo('Syntaxe JSON')
  await fill('Valeur à mémoriser', '{"count":')
  await finishStep()
  await expect.poll(async () => nativeNodes(await screen()).some(node => node.attributes.text === 'Enregistrer' && !node.enabled)).toBe(true)
  expect((await workspace.api(`/api/hook-rules/${waiting.id}`)).version).toBe(2)
  await scrollTo('Syntaxe JSON')
  // The exact invalid text must survive dismissing/reopening its native sheet.
  await expect.poll(async () => nativeNodes(await screen()).some(node => node.attributes.class === 'android.widget.EditText' && node.attributes.text === '{"count":')).toBe(true)
  await fill('Valeur à mémoriser', '{"count":2}')
  await finishStep()
  await tap('Enregistrer')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${waiting.id}`)).version).toBe(3)
  expect((await workspace.api(`/api/hook-rules/${waiting.id}`)).definition.steps[1].action.value).toEqual({ type: 'literal', value: { count: 2 } })
  await adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await tap('Support messages mobile')
  await scrollTo('Historique')
  await scrollTo('Terminé · 1 actions')
  await scrollTo('Résultat ⌄')
  await adb('shell', 'input', 'swipe', '360', '1000', '360', '500', '450')
  await expect.poll(async () => (await screen()).includes('Received: A real local rule execution')).toBe(true)
  await capture(info.outputPath('android-history.png'))
})
test('a native member selects people and a channel through the execution agent', async ({ page, browser, workspace }, info) => {
  test.setTimeout(300000)
  expect((await adb('shell', 'getprop', 'ro.kernel.qemu')).trim()).toBe('1')
  expect(await adb('shell', 'wm', 'size')).toContain('720x1280')
  const fixture = await rulesFixture(workspace)
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  const owner = await (await page.request.get(`${workspace.url}/api/account/session`)).json()
  const email = `native-rule-member-${crypto.randomUUID()}@example.test`
  const invitation = await page.request.post(`${workspace.url}/api/installations/${workspace.installationId}/sharing/invitations`, {
    headers: { 'origin': workspace.url, 'x-csrf-token': owner.csrf },
    data: { email },
  })
  expect(invitation.ok()).toBe(true)
  const memberContext = await browser.newContext({ serviceWorkers: 'block' })
  try {
    const member = await memberContext.newPage()
    await member.goto(workspace.url)
    await member.getByLabel('Email address', { exact: true }).fill(email)
    const previousCode = workspace.emailProof().code
    await member.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => workspace.emailProof().code).not.toBe(previousCode)
    const firstCode = workspace.emailProof().code
    const emailSentAt = Date.now()
    await member.getByLabel('Email code', { exact: true }).fill(firstCode)
    await member.getByRole('button', { name: 'Sign in', exact: true }).click()
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(`${workspace.url}/installations/${workspace.installationSlug}/`)
    const session = await (await member.request.get(`${workspace.url}/api/account/session`)).json()
    const accountId = session.account.id
    await workspace.api(`/api/agents/${fixture.agent.id}/accounts/${accountId}`, 'PUT', { agentId: fixture.agent.id, accountId })
    const definition = structuredClone(fixture.definition)
    definition.name = 'Member choices'
    definition.steps[0].then[0].label = 'Member channel'
    const created = await member.request.post(`${workspace.url}/api/installations/${workspace.installationId}/api/hook-rules`, {
      headers: { 'origin': workspace.url, 'x-csrf-token': session.csrf },
      data: { definition, enabled: false },
    })
    const rule = await created.json()
    expect(created.ok(), rule.error).toBe(true)
    expect(rule.creatorAccountId).toBe(accountId)
    expect(rule.definition.name).toBe('Member choices')
    const createdAuthor = await member.request.post(`${workspace.url}/api/installations/${workspace.installationId}/api/hook-rules`, {
      headers: { 'origin': workspace.url, 'x-csrf-token': session.csrf },
      data: {
        definition: {
          ...fixture.definition,
          name: 'Member conversation author',
          steps: [{
            type: 'action',
            id: 'start',
            label: 'Member author',
            action: { type: 'startAgent', agentId: fixture.agent.id, prompt: { type: 'template', text: 'Answer the message.' } },
          }],
        },
        enabled: false,
      },
    })
    const authorRule = await createdAuthor.json()
    expect(createdAuthor.ok(), authorRule.error).toBe(true)
    expect(authorRule.creatorAccountId).toBe(accountId)
    const port = new URL(workspace.url).port
    await adb('reverse', `tcp:${port}`, `tcp:${port}`)
    await adb('shell', 'am', 'force-stop', 'build.cairn.app')
    if (process.env.CAIRN_RULES_ANDROID_RESET !== '1')
      throw new Error('The member journey requires CAIRN_RULES_ANDROID_RESET=1 on its disposable emulator.')
    await adb('shell', 'pm', 'clear', 'build.cairn.app')
    await adb('shell', 'am', 'start', '-n', 'build.cairn.app/.MainActivity')
    await fill('Adresse e-mail', email)
    await expect.poll(() => Date.now() - emailSentAt, { timeout: 70000, intervals: [2000] }).toBeGreaterThan(62000)
    await tap('Recevoir un code')
    await expect.poll(() => workspace.emailProof().code).not.toBe(firstCode)
    await fill('Code reçu par e-mail', () => workspace.emailProof().code)
    await tap('Se connecter')
    await tap('Atelier')
    await tap('Règles')
    await scrollTo('Member choices')
    await scrollTo('Modifier')
    await scrollTo('Si / sinon')
    await scrollTo('Choisir une personne…')
    await expect.poll(async () => (await screen()).includes('Bob'), { timeout: 15000 }).toBe(true)
    await capture(info.outputPath('android-member-people.png'))
    await tap('Bob')
    await finishStep()
    await scrollTo('Member channel')
    await scrollTo('Choisir une ressource')
    await expect.poll(async () => (await screen()).includes('#general'), { timeout: 15000 }).toBe(true)
    await capture(info.outputPath('android-member-channels.png'))
    await tap('#general')
    await finishStep()
    await tap('Enregistrer')
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).version).toBe(2)
    const saved = await workspace.api(`/api/hook-rules/${rule.id}`)
    expect(saved.creatorAccountId).toBe(accountId)
    expect(saved.definition.steps[0].condition.people).toContainEqual({ resourceType: 'person', id: 'bob' })
    expect(saved.definition.steps[0].then[0].action.input.channel).toEqual({ type: 'literal', value: 'general' })
    await expect.poll(async () => {
      const document = await screen()
      return !!positionIn(document, 'Modifier') && !positionIn(document, 'Enregistrer')
    }, { timeout: 25000 }).toBe(true)
    await capture(info.outputPath('android-member-saved.png'))
    await adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
    await expect.poll(async () => (await screen()).includes('content-desc="Close sheet"'), { timeout: 25000 }).toBe(false)
    await scrollTo('Member conversation author')
    await scrollTo('Modifier')
    await expect.poll(async () => nativeNodes(await screen()).some(node => node.attributes.text === 'Enregistrer'), { timeout: 25000 }).toBe(true)
    await scrollTo('Member author')
    await scrollTo('Auteur externe (facultatif)')
    await fill('Auteur externe (facultatif)', 'payload.author')
    await capture(info.outputPath('android-member-start-author-field.png'))
    await finishStep()
    await tap('Enregistrer')
    await expect.poll(async () => (await workspace.api(`/api/hook-rules/${authorRule.id}`)).version).toBe(2)
    const authored = await workspace.api(`/api/hook-rules/${authorRule.id}`)
    expect(authored.creatorAccountId).toBe(accountId)
    expect(authored.definition.steps[0].action.author).toEqual({ type: 'formula', expression: 'payload.author' })
    await capture(info.outputPath('android-member-start-author.png'))
  }
  finally {
    await memberContext.close()
  }
})
