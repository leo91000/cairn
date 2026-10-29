import type {} from '../../scripts/fixtures/questionnaire'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { createServer } from 'vite'

let server: Awaited<ReturnType<typeof createServer>>
let address: string
test.beforeAll(async () => {
  server = await createServer({
    configFile: false,
    root: process.cwd(),
    plugins: [vue(), tailwindcss()],
    server: { host: '127.0.0.1', port: 0 },
    logLevel: 'error',
  })
  await server.listen()
  address = server.resolvedUrls!.local[0]!
})
test.afterAll(async () => server?.close())
test.setTimeout(60_000)

for (const width of [1440, 390]) {
  test(`multiple choices keep their comment and enforce the complete group at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 })
    let answer: any
    await page.route('**/api/chats/chat/questions/question/answer', async (route) => {
      answer = route.request().postDataJSON()
      await route.fulfill({ json: { status: 'answering' } })
    })
    await page.goto(`${address}scripts/fixtures/questionnaire.html`)
    await page.waitForFunction(() => window.questionnaire)
    await page.evaluate(() => window.questionnaire([
      {
        id: 'platforms',
        title: 'Which platforms?',
        mode: 'multiple',
        minChoices: 2,
        maxChoices: 2,
        options: [
          { id: 'android', label: 'Android', recommended: true },
          { id: 'web', label: 'Web' },
          { id: 'desktop', label: 'Desktop' },
        ],
      },
      { id: 'next', title: 'Next step?' },
    ]))
    await page.getByRole('button', { name: 'Answer', exact: true }).click()
    const send = page.getByRole('button', { name: 'Send answer', exact: true })
    await expect(page.getByRole('checkbox', { name: /Android/ })).not.toBeChecked()
    await expect(page.getByText('Recommended', { exact: true })).toBeVisible()
    await expect(send).toBeDisabled()
    await page.getByRole('checkbox', { name: /Android/ }).check()
    await page.getByRole('textbox', { name: /Comment or your own answer/ }).fill('Android first')
    await page.getByRole('textbox', { name: 'Your answer', exact: true }).fill('Ship it')
    await expect(page.getByRole('checkbox', { name: /Android/ })).toBeChecked()
    await expect(send).toBeDisabled()
    await expect(page.getByText('1 selected · Choose 1 more', { exact: true })).toBeVisible()
    await page.getByRole('checkbox', { name: 'Web', exact: true }).check()
    await expect(send).toBeEnabled()
    await page.getByRole('checkbox', { name: 'Desktop', exact: true }).check()
    await expect(send).toBeDisabled()
    await expect(page.getByText('3 selected · Remove 1', { exact: true })).toBeVisible()
    await page.getByRole('checkbox', { name: 'Desktop', exact: true }).uncheck()
    await send.click()
    await expect(page.getByRole('dialog')).not.toBeVisible()
    expect(answer.answers).toEqual({ platforms: { selected: ['android', 'web'], comment: 'Android first' }, next: { selected: [], comment: 'Ship it' } })
  })
}

test('single choice supports a comment and can be cleared for a text-only alternative', async ({ page }) => {
  await page.goto(`${address}scripts/fixtures/questionnaire.html`)
  await page.waitForFunction(() => window.questionnaire)
  await page.evaluate(() => window.questionnaire([{ id: 'plan', title: 'Which plan?', options: [{ id: 'small', label: 'Small' }, { id: 'large', label: 'Large' }] }]))
  await page.getByRole('button', { name: 'Answer', exact: true }).click()
  await page.getByRole('radio', { name: 'Small', exact: true }).check()
  await page.getByRole('textbox').fill('A different plan')
  await expect(page.getByRole('radio', { name: 'Small', exact: true })).toBeChecked()
  await page.getByRole('button', { name: 'Clear choices' }).click()
  await expect(page.getByRole('radio', { name: 'Small', exact: true })).not.toBeChecked()
  await expect(page.getByRole('textbox')).toHaveValue('A different plan')
  await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeEnabled()
})

test('submitted answers have a readable conversation summary', async ({ page }) => {
  await page.goto(`${address}scripts/fixtures/questionnaire.html`)
  await page.waitForFunction(() => window.questionnaireHistory)
  await page.evaluate(() => window.questionnaireHistory(`My answers to your questions:\n\n${JSON.stringify([{
    questionId: 'platforms',
    question: 'Which platforms?',
    selected: [{ id: 'android', label: 'Android' }, { id: 'web', label: 'Web' }],
    comment: 'Android first\nWeb next',
  }])}`))
  const bubble = page.locator('.user-bubble')
  await expect(bubble).toContainText('Choices: Android, Web')
  await expect(bubble).toContainText('Comment: Android first\nWeb next')
  await expect(bubble).not.toContainText('questionId')
  await expect(bubble).not.toContainText('selected')
})

test('raw options without a mode use single selection for both cards and submission', async ({ page }) => {
  let answer: any
  await page.route('**/api/chats/chat/questions/question/answer', async (route) => {
    answer = route.request().postDataJSON()
    await route.fulfill({ json: { status: 'answering' } })
  })
  await page.goto(`${address}scripts/fixtures/questionnaire.html`)
  await page.waitForFunction(() => window.questionnaireRaw)
  await page.evaluate(() => window.questionnaireRaw([{
    id: 'filters',
    title: 'Which editor?',
    options: [
      { id: 'structured', label: 'Structured editor', description: 'Groups and filters' },
      { id: 'expressions', label: 'Editor with expressions', description: 'Expressions too' },
      { id: 'simple', label: 'Simple filters', description: 'Conditions only' },
    ],
  }]))
  await page.getByRole('button', { name: 'Answer', exact: true }).click()
  await page.getByText('Structured editor', { exact: true }).click()
  await expect(page.getByRole('radio', { name: 'Structured editor Groups and filters', exact: true })).toBeChecked()
  await page.getByText('Simple filters', { exact: true }).click()
  await expect(page.getByRole('radio', { name: 'Structured editor Groups and filters', exact: true })).not.toBeChecked()
  await expect(page.getByRole('radio', { name: 'Simple filters Conditions only', exact: true })).toBeChecked()
  await expect(page.getByText('Choose one option, or answer using text only')).toBeVisible()
  await page.getByRole('button', { name: 'Send answer', exact: true }).click()
  expect(answer.answers.filters).toEqual({ selected: ['simple'], comment: '' })
})

for (const invalid of ['missing IDs', 'duplicate IDs']) {
  test(`unsupported questions with ${invalid} cannot produce an ambiguous answer`, async ({ page }) => {
    await page.goto(`${address}scripts/fixtures/questionnaire.html`)
    await page.waitForFunction(() => window.questionnaireRaw)
    await page.evaluate(invalid => window.questionnaireRaw([{
      id: 'filters',
      title: 'Which editor?',
      options: [
        { ...(invalid === 'duplicate IDs' ? { id: 'same' } : {}), label: 'Structured editor' },
        { ...(invalid === 'duplicate IDs' ? { id: 'same' } : {}), label: 'Simple filters' },
      ],
    }]), invalid)
    await page.getByRole('button', { name: 'Answer', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Update the Cairn installation')
    await expect(page.getByRole('radio')).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeDisabled()
    await page.route('**/api/chats/chat/questions/question/cancel', route => route.fulfill({ json: { status: 'cancelled' } }))
    await page.getByRole('button', { name: 'No longer needed', exact: true }).click()
    await expect(page.getByRole('dialog')).not.toBeVisible()
  })
}
