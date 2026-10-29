import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'

test('readable URLs preserve legacy deep links, UUID drafts and conversation identity', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  const other = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, title: 'Readable conversation' })
  workspace.service.store.put('chats', { ...other, title: 'Another conversation' })

  // Titles are not identities: other journeys may leave conversations with either title.
  for (const title of ['Readable conversation', 'Another conversation']) {
    const duplicate = await workspace.api('/api/chats', 'POST', {})
    workspace.service.store.put('chats', { ...duplicate, title })
  }

  expect(workspace.installationSlug).toMatch(/^[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  expect(chat.slug).toMatch(/^[a-z]+-[a-z]+-[a-f0-9]{8}$/)

  const legacy = `/installations/${workspace.installationId}/chats/${chat.id}?source=bookmark#reply`
  await page.goto(legacy)
  await signIn(page)
  await expect(page).toHaveURL(`${workspace.url}${workspacePath(`/chats/${chat.id}?source=bookmark#reply`)}`)
  await expect(page.getByRole('heading', { name: 'Readable conversation', exact: true })).toBeVisible()
  const message = page.getByLabel('Message', { exact: true })
  await message.fill('A draft saved from the old UUID link')
  await page.goto(workspacePath(`/chats/${chat.slug}`))
  await expect(message).toHaveValue('A draft saved from the old UUID link')

  const details = page.getByRole('button', { name: 'Chat details', exact: true })
  const search = page.getByRole('button', { name: 'Search conversations', exact: true })
  const conversations = page.getByRole('dialog', { name: /Conversations/ })

  await details.click()
  await search.click()
  await conversations.locator(`a[href="${workspacePath(`/chats/${other.slug}`)}"]`).click()
  await expect(page).toHaveURL(`${workspace.url}${workspacePath(`/chats/${other.slug}`)}`)
  await message.fill('A separate draft')
  await details.click()
  await search.click()
  await conversations.locator(`a[href="${workspacePath(`/chats/${chat.slug}`)}"]`).click()
  await expect(page).toHaveURL(`${workspace.url}${workspacePath(`/chats/${chat.slug}`)}`)
  await expect(message).toHaveValue('A draft saved from the old UUID link')
  await page.reload()
  await expect(message).toHaveValue('A draft saved from the old UUID link')

  await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
  await expect(page.locator('.activity-message').getByText('A draft saved from the old UUID link', { exact: true })).toBeVisible()
  expect((await workspace.api(`/api/chats/${chat.id}`)).id).toBe(chat.id)
  expect((await workspace.api(`/api/chats/${chat.slug}`)).slug).toBe(chat.slug)
  await page.reload()
  await expect(page.getByRole('heading', { name: 'A draft saved from the old UUID link', exact: true })).toBeVisible()
  await expect(page).toHaveURL(`${workspace.url}${workspacePath(`/chats/${chat.slug}`)}`)
})
