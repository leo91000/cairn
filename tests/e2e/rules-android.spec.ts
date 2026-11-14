import { execFileSync } from 'node:child_process'
import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { rulesFixture } from './rules-fixture'

// Opt-in journey against the real installation and Beacon. Use a disposable emulator:
// ANDROID_SERIAL=emulator-5580 CAIRN_RULES_ANDROID=1 pnpm test:e2e --project=rules-android
// Build/install the debug APK with CAIRN_BEACON_ORIGIN=http://localhost:4422 first.
// CAIRN_RULES_ANDROID_RESET=1 clears only this emulator's Cairn app before sign-in.
const serial = process.env.ANDROID_SERIAL
const binary = path.join(process.env.ANDROID_HOME ?? '', 'platform-tools/adb')

function adb(...args: string[]) {
  if (!serial?.startsWith('emulator-'))
    throw new Error('This journey needs ANDROID_SERIAL selecting a disposable emulator.')
  return execFileSync(binary, ['-s', serial, ...args], { encoding: 'utf8', timeout: 15000 })
}

function screen() {
  adb('shell', 'uiautomator', 'dump', '/sdcard/cairn-rules-journey.xml')
  return adb('exec-out', 'cat', '/sdcard/cairn-rules-journey.xml')
}

function positionIn(document: string, text: string, includeClipped = false): number[] | null {
  for (const node of document.match(/<node\b[^>]+>/g) ?? []) {
    if (node.match(/\btext="([^"]*)"/)?.[1] !== text && node.match(/content-desc="([^"]*)"/)?.[1] !== text)
      continue
    if (node.includes('enabled="false"'))
      continue
    const match = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/)
    if (!match)
      continue
    const bounds = match.slice(1).map(Number)
    if (bounds[2] - bounds[0] < 20 || bounds[3] - bounds[1] < (includeClipped ? 1 : 30))
      continue
    const center = [Math.floor((bounds[0] + bounds[2]) / 2), Math.floor((bounds[1] + bounds[3]) / 2)]
    if (center[1] < 64 || center[1] > 1160)
      continue
    return center
  }

  return null
}

function tapPosition(position: number[]) {
  adb('shell', 'input', 'tap', String(position[0]), String(position[1]))
}

async function tap(text: string) {
  let previous = ''
  await expect.poll(() => {
    const current = screen()
    if (text !== 'Plus tard' && current.includes('Mise à jour disponible')) {
      const postpone = positionIn(current, 'Plus tard')
      if (postpone)
        tapPosition(postpone)
      previous = ''
      return false
    }

    const position = positionIn(current, text)
    const signature = position?.join(',') ?? ''
    if (!position || signature !== previous) {
      previous = signature
      return false
    }

    tapPosition(position)
    return true
  }, { timeout: 25000, intervals: [500] }).toBe(true)
}

async function scrollTo(text: string) {
  for (let attempt = 0; attempt < 7; attempt++) {
    const current = screen()
    if (positionIn(current, text)) {
      await tap(text)
      return
    }

    const clipped = positionIn(current, text, true)
    if (clipped && clipped[1] < 400)
      adb('shell', 'input', 'swipe', '360', '500', '360', '700', '350')
    else
      adb('shell', 'input', 'swipe', '360', '900', '360', '450', '450')
  }

  throw new Error(`The native screen did not expose ${text}.`)
}

async function fill(label: string, value: string | (() => string)) {
  await tap(label)
  adb('shell', 'input', 'keycombination', 'KEYCODE_CTRL_LEFT', 'KEYCODE_A')
  adb('shell', 'input', 'text', (typeof value === 'function' ? value() : value).replaceAll(' ', '%s'))
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
}

async function capture(file: string) {
  await writeFile(file, execFileSync(binary, ['-s', serial!, 'exec-out', 'screencap', '-p'], { timeout: 15000 }))
}

test('native rules persist edits, integer waits and every creator control against the real service', async ({ page, workspace }, info) => {
  test.setTimeout(360000)
  expect(adb('shell', 'getprop', 'ro.kernel.qemu').trim()).toBe('1')
  expect(adb('shell', 'wm', 'size')).toContain('720x1280')
  const started = Date.now()
  const fixture = await rulesFixture(workspace)
  const rule = await workspace.api('/api/hook-rules', 'POST', {
    definition: { ...fixture.definition, limits: { triggeringsPerHour: 1, conversationsPerHour: 5 } },
    enabled: true,
  })
  workspace.service.emitHook(fixture.instanceId, 'message', { author: 'alice', channel: 'support', content: 'A real local rule execution' }, 'native-first')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}/triggerings`)).triggerings[0]?.status).toBe('completed')
  await page.goto(workspacePath(`/rules?rule=${rule.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Edit', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Save version', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Edit Acknowledge the message', exact: true }).click()
  await expect(page.getByText('#support', { exact: true }).first()).toBeVisible()
  await page.screenshot({ path: info.outputPath('web-editor.png'), animations: 'disabled' })
  const port = new URL(workspace.url).port
  adb('reverse', `tcp:${port}`, `tcp:${port}`)
  adb('shell', 'am', 'force-stop', 'build.cairn.app')
  if (process.env.CAIRN_RULES_ANDROID_RESET === '1')
    adb('shell', 'pm', 'clear', 'build.cairn.app')
  adb('shell', 'am', 'start', '-n', 'build.cairn.app/.MainActivity')
  await fill('Adresse e-mail', workspace.emailProof().email)
  // Keep Beacon's actual per-address email delivery cooldown.
  await expect.poll(() => Date.now() - started, { timeout: 70000, intervals: [2000] }).toBeGreaterThan(62000)
  await tap('Recevoir un code')
  await fill('Code reçu par e-mail', () => workspace.emailProof().code)
  await tap('Se connecter')
  await tap('Atelier')
  await tap('Règles')
  await expect.poll(() => screen().includes('Support messages')).toBe(true)
  await capture(info.outputPath('android-list.png'))
  await tap('Support messages')
  await tap('Modifier')
  await fill('Nom de la règle', 'Support messages mobile')
  await capture(info.outputPath('android-editor.png'))
  adb('shell', 'input', 'swipe', '360', '1000', '360', '300', '600')
  await capture(info.outputPath('android-flow.png'))
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
  adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await tap('Support messages mobile')
  await scrollTo('Relancer le budget')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).revision).not.toBe(exhaustedRevision)
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).blocks).toEqual([])
  await scrollTo('Dupliquer')
  await expect.poll(async () => (await workspace.api('/api/hook-rules')).rules.length).toBe(2)
  const copy = (await workspace.api('/api/hook-rules')).rules.find((item: any) => item.id !== rule.id)
  expect(copy.blocks.map((block: any) => block.kind)).toContain('disabled')
  await expect.poll(() => screen().includes(copy.name)).toBe(true)
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
  for (let step = 0; step < 2 && !screen().includes('Enregistrer'); step++)
    adb('shell', 'input', 'keyevent', 'KEYCODE_BACK')
  await tap('Enregistrer')
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${waiting.id}`)).version).toBe(2)
  expect((await workspace.api(`/api/hook-rules/${waiting.id}`)).definition.steps[0].action.seconds).toEqual({ type: 'literal', value: 900 })
})
