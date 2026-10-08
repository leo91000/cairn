import { Buffer } from 'node:buffer'
import { readFileSync } from 'node:fs'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  expectSingleScroll,
  initializeRepository,
  test,
  workspacePath,
} from './fixtures'

test('uploads images and files, previews them and preserves attachments while editing the queue', async ({ page, workspace }) => {
  initializeRepository(workspace.projectPath)
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  await page.goto(workspacePath('/chats'))
  const image = { name: 'design.png', mimeType: 'image/png', buffer: readFileSync('docs/screenshots/overview-desktop.png') }
  const notes = { name: 'review-notes.md', mimeType: 'text/markdown', buffer: Buffer.from('# Design review\nMake the controls easier to reach on mobile.') }
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([image])
  await page.locator('form').evaluate((form) => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['# Design review\nMake the controls easier to reach on mobile.'], 'review-notes.md', { type: 'text/markdown' }))
    form.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }))
  })
  await page.getByRole('textbox', { name: 'Message', exact: true }).evaluate((textarea) => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['fixture'], 'pasted.png', { type: 'image/png' }))
    textarea.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }))
  })
  await page.getByRole('button', { name: 'Remove pasted.png' }).click()
  await expect(page.getByRole('button', { name: 'Preview design.png', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Preview design.png', exact: true }).click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.press('Escape')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Compare this design with the notes. fixture:chat-hang')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('attachments-desktop-light.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-f0-9-]+$/)
  const chatId = page.url().split('/').at(-1)!
  const user = page.locator('.activity-message').filter({ hasText: 'Compare this design' })
  await expect(user.getByRole('button', { name: 'Preview design.png', exact: true })).toBeVisible()
  const download = user.getByRole('link', { name: 'Download review-notes.md' })
  const response = await page.request.get((await download.getAttribute('href'))!)
  expect(await response.text()).toContain('# Design review')
  expect(response.headers()['content-disposition']).toContain('attachment;')
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([image])
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Remove design.png' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Review this image next.')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  const detail = await workspace.api(`/api/chats/${chatId}`)
  expect(detail.messages.at(-1).attachments).toHaveLength(1)
  await expect(page.getByLabel('Attach files', { exact: true })).toBeEnabled()
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{ ...notes, name: 'follow-up.md' }, { ...image, name: 'mobile-design.png' }])
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Use this extra context.')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.mouse.move(0, 0)
  await expect(page.getByRole('button', { name: 'Steer now', exact: true })).toBeInViewport()
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('attachments-mobile-dark.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Queue', exact: true })).toBeInViewport()
  await page.getByRole('button', { name: 'Steer now', exact: true }).click()
  await expect(page.locator('.activity-message').getByRole('link', { name: 'Download follow-up.md' })).toBeVisible()
  await page.reload()
  await expect(page.locator('.activity-message').getByRole('button', { name: 'Preview design.png', exact: true })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Steer now', exact: true }).click()
  await expectChatReady(page)
  await expect.poll(async () => (await workspace.api(`/api/chats/${chatId}`)).messages.every((m: { status: string }) => m.status === 'delivered')).toBe(true)
  await expect(page.getByLabel('Attach files', { exact: true })).toBeEnabled()
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{ ...image, name: 'image-only.png' }])
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.locator('.activity-message').getByRole('button', { name: 'Preview image-only.png', exact: true })).toBeVisible()
  await expectChatReady(page)
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{ name: 'large.dat', mimeType: 'application/octet-stream', buffer: Buffer.alloc(10 * 1024 * 1024 + 1) }])
  await expect(page.getByText('Files must be 10 MB or smaller.', { exact: true })).toBeVisible()
})
