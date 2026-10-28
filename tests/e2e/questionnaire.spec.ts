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
    await page.getByRole('checkbox', { name: 'Web', exact: true }).check()
    await expect(send).toBeEnabled()
    await page.getByRole('checkbox', { name: 'Desktop', exact: true }).check()
    await expect(send).toBeDisabled()
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
