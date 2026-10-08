import type { Deliverable } from '../../shared/artifacts'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  initializeRepository,
  test,
  workspacePath,
} from './fixtures'

test('persistent deliverables have a gallery, revisions, mobile viewer, and playable media', async ({ page, browser, workspace }) => {
  test.setTimeout(60000)
  initializeRepository(workspace.projectPath)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Prepare a mobile review with screenshots and a short walkthrough.' })
  let detail: any
  await expect.poll(async () => {
    detail = await workspace.api(`/api/chats/${chat.id}`)
    return detail.run?.status
  }).toBe('succeeded')
  const runId = detail.run.id
  const directory = path.join(workspace.service.config.dataDir, 'artifacts')
  await mkdir(directory, { recursive: true })
  const image = await readFile('docs/screenshots/model-selectors/chat-mobile-dark.png')
  const files: Deliverable[] = []
  const publish = async (title: string, name: string, kind: Deliverable['kind'], bytes: Uint8Array, key = title, version = 1) => {
    const item: Deliverable = {
      id: randomUUID(),
      runId,
      messageId: null,
      key,
      version,
      title,
      name,
      kind,
      size: bytes.length,
      mediaType: ({
        image: 'image/png',
        video: 'video/mp4',
        audio: 'audio/wav',
        pdf: 'application/pdf',
      } as Record<string, string>)[kind] || 'text/plain',
      createdAt: Date.now(),
      url: '',
      group: 'Mobile review',
      previewStatus: 'none',
    }
    if (kind === 'markdown' || kind === 'code')
      item.excerpt = new TextDecoder().decode(bytes).slice(0, 400)
    files.push(item)
    await writeFile(path.join(directory, item.id), bytes)
    workspace.service.store.set(`artifact:${runId}:${item.id}`, item)
    workspace.service.store.event(runId, 'artifact', title, { ...item })
    return item
  }

  await publish('Mobile · before', 'mobile-before.png', 'image', image, 'mobile', 1)
  const second = await publish('Mobile · refined', 'mobile.png', 'image', await readFile('docs/screenshots/chat-questions/questions-mobile-dark.png'), 'mobile', 2)
  await publish('Review notes', 'review.md', 'markdown', new TextEncoder().encode('# Mobile review\n\nThe controls stay within reach.\n\n- More room for the conversation\n- A clear place for every deliverable\n\n**Ready for your feedback.**'))
  await publish('Component changes', 'chat.ts', 'code', new TextEncoder().encode('export const preview = {\n  fullscreen: true,\n  persistent: true,\n}\n'))
  const video = await publish('A quick walkthrough', 'walkthrough.mp4', 'video', await readFile('tests/fixtures/artifacts/walkthrough.mp4'))
  await publish('Audio notes', 'notes.wav', 'audio', await readFile('tests/fixtures/artifacts/notes.wav'))
  await publish('Printable review', 'review.pdf', 'pdf', await readFile('tests/fixtures/artifacts/review.pdf'))
  const notes = files.find(item => item.kind === 'markdown')!
  const answer = `The review is ready. [Read the report](/api/runs/${runId}/artifacts/${notes.id}) · [View image](${workspace.url}/api/installations/${workspace.installationId}/api/runs/${runId}/artifacts/${second.id}) · [Documentation](https://example.test/docs) · [Missing file](/api/runs/${runId}/artifacts/missing)`
  workspace.service.store.event(runId, 'item.completed', answer, { type: 'item.completed', item: { id: 'deliverable-answer', type: 'agent_message', text: answer } })
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('button', { name: 'Files · 6', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Open Mobile · refined', exact: true })).toBeVisible()
  // Message links open the authenticated viewer; external links keep browser behavior.
  await page.getByRole('link', { name: 'Read the report', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Deliverables viewer' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Mobile review', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Share file', exact: true }).click()
  const sharing = page.getByRole('region', { name: 'File sharing' })
  await sharing.getByRole('button', { name: 'Enable public link', exact: true }).click()
  const link = sharing.getByRole('textbox', { name: 'Public link', exact: true })
  await expect(link).toBeVisible()
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('public-sharing-desktop.png') })
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(sharing.getByRole('button', { name: 'Disable public link', exact: true })).toBeInViewport()
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('public-sharing-mobile.png') })
  await page.setViewportSize({ width: 1440, height: 1000 })
  const publicUrl = await link.inputValue()
  const guest = await browser.newContext()
  try {
    const response = await guest.request.get(publicUrl)
    expect(response.status()).toBe(200)
    expect(await response.text()).toContain('Mobile review')
    expect((await guest.request.get(`${workspace.url}/api/installations/${workspace.installationId}/api/runs/${runId}/artifacts/${notes.id}`)).status()).toBe(401)
    await sharing.getByRole('button', { name: 'Disable public link', exact: true }).click()
    await expect(sharing.getByText('Private file', { exact: true })).toBeVisible()
    expect((await guest.request.get(publicUrl)).status()).toBe(404)
    await sharing.getByRole('button', { name: 'Enable public link', exact: true }).click()
    await expect(link).toBeVisible()
    expect(await link.inputValue()).not.toBe(publicUrl)
    expect((await guest.request.get(publicUrl)).status()).toBe(404)
    await sharing.getByRole('button', { name: 'Disable public link', exact: true }).click()
    await expect(sharing.getByText('Private file', { exact: true })).toBeVisible()
  }
  finally { await guest.close() }

  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await page.getByRole('link', { name: 'View image', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('img').first()).toBeVisible()
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await page.context().route('https://example.test/docs', route => route.fulfill({ contentType: 'text/html', body: '<h1>Documentation</h1>' }))
  const popupPromise = page.waitForEvent('popup')
  await page.getByRole('link', { name: 'Documentation', exact: true }).click()
  const popup = await popupPromise
  await expect(popup.getByRole('heading', { name: 'Documentation' })).toBeVisible()
  await popup.close()
  await page.getByRole('link', { name: 'Missing file', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('This file is no longer available.')
  await page.getByRole('link', { name: 'Read the report', exact: true }).click()
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await expectSingleScroll(page)
  await expect.poll(() => page.locator('img').evaluateAll(images => images.every(image => (image as HTMLImageElement).complete && (image as HTMLImageElement).naturalWidth > 0))).toBe(true)
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-desktop-dark.png') })
  await page.getByRole('button', { name: 'Open Mobile · refined', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Deliverables viewer' })).toBeVisible()
  await page.getByRole('button', { name: 'Compare', exact: true }).click()
  await page.getByRole('slider', { name: 'Comparison position' }).fill('35')
  await workspace.api(`/api/chats/${chat.id}/pause`, 'POST', { paused: true })
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
  await expect(page.getByRole('slider', { name: 'Comparison position' })).toHaveValue('35')
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-comparison.png') })
  await page.getByRole('button', { name: 'All deliverables', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Open Review notes', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Mobile review', exact: true })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-document-mobile-dark.png') })
  await page.getByRole('button', { name: 'All deliverables', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Open Printable review', exact: true }).click()
  await expect(page.getByRole('img', { name: 'Printable review, page 1', exact: true })).toHaveAttribute('aria-busy', 'false')
  await expect(page.getByRole('alert')).not.toBeVisible()
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-pdf-mobile.png') })

  await page.getByRole('button', { name: 'All deliverables', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Open A quick walkthrough', exact: true }).click()
  await expect(page.locator('video')).toBeVisible()
  await expect.poll(() => page.locator('video').evaluate((v: HTMLVideoElement) => v.readyState)).toBeGreaterThan(0)
  await page.locator('video').evaluate(async (v: HTMLVideoElement) => {
    v.muted = true
    await v.play()
    v.currentTime = 1
  })
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-video-mobile.png') })
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await page.emulateMedia({ colorScheme: 'light' })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('button', { name: 'Files 6', exact: true }).click()
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('deliverables-gallery-mobile-light.png') })
  await page.setViewportSize({ width: 320, height: 600 })
  expect(await page.getByRole('dialog').evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true)
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await page.reload()
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Files 6', exact: true })).toBeVisible()
  const content = await page.request.get(`/api/installations/${workspace.installationId}/api/runs/${runId}/artifacts/${video.id}`, { headers: { Range: 'bytes=0-31' } })
  expect(content.status()).toBe(206)
  expect((await content.body()).length).toBe(32)
  const download = await page.request.get(`/api/installations/${workspace.installationId}/api/runs/${runId}/artifacts/${second.id}?download=1`)
  expect(download.headers()['content-disposition']).toContain('attachment;')
  await page.goto(workspacePath(`/runs/${runId}`))
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await page.getByRole('link', { name: 'Read the report', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Mobile review', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
  await page.getByRole('button', { name: 'Result', exact: true }).click()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(page.getByRole('button', { name: 'Open Mobile · refined', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Open Mobile · refined', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Deliverables viewer' })).toBeVisible()
  await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
})
