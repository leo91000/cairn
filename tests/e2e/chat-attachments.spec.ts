import { Buffer } from 'node:buffer'
import { readFileSync } from 'node:fs'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  expectSingleScroll,
  initializeRepository,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('sent SVGs remain images in the mosaic, viewer and clipboard', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  let legacyRequests = 0
  await page.route(/\/chats\/[^/]+\/attachments\/[^/?]+\/stream$/, (route) => {
    legacyRequests++
    return route.fulfill({ status: 404, json: { error: 'Not found' } })
  })
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        write: async (items: ClipboardItem[]) => {
          const png = await items[0].getType('image/png')
          document.documentElement.dataset.copiedType = png.type
        },
      },
    })
  })
  initializeRepository(workspace.projectPath)
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{
    name: 'logo.svg',
    mimeType: 'image/svg+xml',
    buffer: Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="20" height="10" fill="red"/><script>document.documentElement.dataset.svgScript="executed"</script></svg>'),
  }])
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Inspect the vector logo.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  await page.reload() // Discard local draft object URLs; exercise the stored attachment.
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()
  await expectChatReady(page)
  const message = page.locator('.activity-message').filter({ hasText: 'Inspect the vector logo.' })
  const thumbnail = message.getByRole('button', { name: 'Preview logo.svg', exact: true })
  await thumbnail.scrollIntoViewIfNeeded() // Stored thumbnails load only when near the viewport.
  await expect.poll(() => thumbnail.locator('img').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(20)
  await thumbnail.click()
  const viewer = page.getByRole('dialog', { name: 'Attachment viewer' })
  await expect.poll(() => viewer.getByLabel('Image preview', { exact: true }).locator('img').evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(20)
  await page.keyboard.press('Escape')
  await message.getByRole('button', { name: 'More actions for logo.svg', exact: true }).click()
  await page.getByRole('menuitem', { name: 'Copy image', exact: true }).click()
  await expect(page.locator('html')).toHaveAttribute('data-copied-type', 'image/png')
  expect(legacyRequests).toBeGreaterThan(0)
  await expect(page.locator('html')).not.toHaveAttribute('data-svg-script', 'executed')
  await message.getByRole('button', { name: 'More actions for logo.svg', exact: true }).click()
  const download = page.waitForEvent('download')
  await page.getByRole('menuitem', { name: 'Download', exact: true }).click()
  expect((await download).suggestedFilename()).toBe('logo.svg')
  await expectChatReady(page)
})

test('uploads images and files, previews them and preserves attachments while editing the queue', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'canShare', { configurable: true, value: () => true })
    Object.defineProperty(navigator, 'share', {
      configurable: true,
      value: (data: ShareData) => {
        if (!navigator.userActivation.isActive)
          return Promise.reject(new DOMException('User activation expired', 'NotAllowedError'))
        document.documentElement.dataset.sharedFile = data.files?.[0]?.name
        return Promise.resolve()
      },
    })
  })
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
  // The ＋ button offers photos, the camera and any file.
  await page.getByRole('button', { name: 'Add images or files', exact: true }).click()
  const attachMenu = page.getByRole('menu', { name: 'Attach' })
  await expect(attachMenu.getByRole('menuitem')).toHaveText(['Photos', 'Camera', 'Files'])
  await page.keyboard.press('Escape')
  await expect(attachMenu).toBeHidden()
  await expect(page.getByRole('button', { name: 'Add images or files', exact: true })).toBeFocused()
  const tray = page.locator('form').getByRole('list', { name: 'Attachments' })
  await expect(tray.getByRole('button', { name: /^Preview / })).toHaveCount(2)
  await expect(tray.getByRole('button', { name: 'Preview review-notes.md', exact: true })).toContainText(/MD · \d+ B/)
  await expect(page.getByRole('button', { name: 'Preview design.png', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Preview design.png', exact: true }).click()
  const viewer = page.getByRole('dialog', { name: 'Attachment viewer' })
  await expect(viewer.getByRole('heading', { name: 'design.png' })).toBeVisible()
  await expect(viewer.getByText(/^PNG · [\d.]+ (KB|MB) · \d+ × \d+$/)).toBeVisible()
  await viewer.getByRole('button', { name: 'Actual size', exact: true }).click()
  const imagePreview = viewer.getByLabel('Image preview', { exact: true })
  await expect.poll(() => imagePreview.evaluate(element => element.scrollWidth > element.clientWidth || element.scrollHeight > element.clientHeight)).toBe(true)
  await viewer.getByRole('button', { name: 'Fit image', exact: true }).click()
  await page.keyboard.press('ArrowRight')
  await expect(viewer.getByRole('heading', { name: 'review-notes.md' })).toBeVisible()
  await expect(viewer.getByText('Uploaded when you send the message')).toBeVisible()
  await viewer.getByRole('button', { name: 'Previous attachment' }).click()
  await expect(viewer.getByRole('heading', { name: 'design.png' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(viewer).toBeHidden()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Compare this design with the notes. fixture:chat-hang')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('attachments-desktop-light.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  const chatId = page.url().split('/').at(-1)!
  const user = page.locator('.activity-message').filter({ hasText: 'Compare this design' })
  await expect(user.getByRole('button', { name: 'Preview design.png', exact: true })).toBeVisible()
  // Attachments sit above the bubble, outside it.
  expect(await user.evaluate(element => element.querySelector('.message-attachments')!.compareDocumentPosition(element.querySelector('.user-bubble')!) & Node.DOCUMENT_POSITION_FOLLOWING)).toBeTruthy()
  await expect(user.getByRole('button', { name: 'Preview review-notes.md', exact: true })).toBeVisible()
  // The optimistic message is replaced once the agent acknowledges it; act on the settled one.
  await expect(page.locator('.activity-message').filter({ hasText: 'trace the component boundaries' })).toBeVisible()
  await expect(user.getByRole('status')).toHaveCount(0)
  // A slow original download must finish before the final Share gesture.
  let delayedOriginals = 0
  await page.route(/\/chats\/[^/]+\/attachments\/[^/?]+\/stream$/, async (route) => {
    delayedOriginals++
    await new Promise(resolve => setTimeout(resolve, 5500))
    await route.continue()
  }, { times: 1 })
  await user.getByRole('button', { name: 'More actions for review-notes.md', exact: true }).click()
  const actions = page.getByRole('menu', { name: 'Actions for review-notes.md' })
  await expect(actions.getByRole('menuitem', { name: 'Reuse in new message' })).toBeVisible()
  const share = actions.getByRole('menuitem', { name: 'Share', exact: true })
  await expect(share).toBeEnabled({ timeout: 15000 })
  await share.click()
  await expect(page.locator('html')).toHaveAttribute('data-shared-file', 'review-notes.md')
  expect(delayedOriginals).toBe(1)
  await user.getByRole('button', { name: 'More actions for review-notes.md', exact: true }).click()
  const download = actions.getByRole('menuitem', { name: 'Download' })
  const response = await page.request.get((await download.getAttribute('href'))!)
  expect(await response.text()).toContain('# Design review')
  expect(response.headers()['content-disposition']).toContain('attachment;')
  await page.keyboard.press('Escape')
  await expect(actions).toBeHidden()
  await user.getByRole('button', { name: 'Preview design.png', exact: true }).click()
  await expect(viewer.getByRole('heading', { name: 'design.png' })).toBeVisible()
  await expect(viewer.getByRole('link', { name: 'Download design.png' })).toBeVisible()
  await expect(viewer.getByRole('button', { name: 'Share design.png', exact: true })).toBeEnabled()
  await viewer.getByRole('button', { name: 'Share design.png', exact: true }).click()
  await expect(page.locator('html')).toHaveAttribute('data-shared-file', 'design.png')
  await expect(viewer.getByRole('list', { name: 'Attachments' }).getByRole('button')).toHaveCount(2)
  await page.keyboard.press('Escape')
  // Reuse puts the same record back in the draft, without uploading it again.
  await user.getByRole('button', { name: 'More actions for review-notes.md', exact: true }).click()
  await actions.getByRole('menuitem', { name: 'Reuse in new message' }).click()
  await expect(tray.getByRole('button', { name: 'Preview review-notes.md', exact: true })).toBeVisible()
  await tray.getByRole('button', { name: 'Remove review-notes.md' }).click()
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([image])
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  // A refused item blocks sending but still counts as an unsent draft.
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{ name: 'refused.dat', mimeType: 'application/octet-stream', buffer: Buffer.alloc(10 * 1024 * 1024 + 1) }])
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Keep this unsent draft')
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Keep this unsent draft')
  await expect(tray.getByRole('button', { name: 'Remove refused.dat', exact: true })).toBeVisible()
  await tray.getByRole('button', { name: 'Remove refused.dat', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('')
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
  await expect(page.locator('.activity-message').getByRole('button', { name: 'Preview follow-up.md', exact: true })).toBeVisible()
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
  // A file over 10 MB stays in the draft with its reason and blocks sending until it is removed.
  await page.getByLabel('Attach files', { exact: true }).setInputFiles([{ name: 'large.dat', mimeType: 'application/octet-stream', buffer: Buffer.alloc(10 * 1024 * 1024 + 1) }])
  await expect(tray.getByText('Too large (10.0 MB > 10 MB)', { exact: true })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Here is the dump')
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeDisabled()
  await tray.getByRole('button', { name: 'Remove large.dat' }).click()
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeEnabled()
})
