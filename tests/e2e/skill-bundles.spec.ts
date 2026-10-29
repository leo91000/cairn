import type { Locator, Page, TestInfo } from '@playwright/test'
import { Buffer } from 'node:buffer'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

const manifest = (name: string, description: string) => `---\nname: ${name}\ndescription: ${description}\n---\n\n# ${name}\n\nRun \`scripts/check.sh\` and read references/notes.md.\n`

// Where a skill imported through a plugin's skill source came from, as the installation records it.
function pluginSource(values: Record<string, unknown> = {}) {
  return {
    pluginId: 'github',
    source: 'repository',
    instance: 'work',
    label: 'acme/skills',
    location: { repository: 'acme/skills', ref: 'main' },
    url: 'https://github.com/acme/skills/tree/main/review',
    folder: 'review',
    version: 'a'.repeat(40),
    importedAt: 1,
    modified: false,
    ...values,
  }
}

// A stored (uncompressed) zip: enough for import fixtures without a dependency.
function crc32(bytes: Uint8Array) {
  let crc = 0xFFFFFFFF
  for (const byte of bytes) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1)
      crc = (crc >>> 1) ^ (0xEDB88320 & -(crc & 1))
  }

  return (crc ^ 0xFFFFFFFF) >>> 0
}

function storedZip(files: Record<string, string>) {
  const local: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0
  for (const [name, value] of Object.entries(files)) {
    const data = Buffer.from(value)
    const fileName = Buffer.from(name)
    const crc = crc32(data)
    const header = Buffer.alloc(30)
    header.writeUInt32LE(0x04034B50, 0)
    header.writeUInt16LE(20, 4)
    header.writeUInt16LE(33, 12)
    header.writeUInt32LE(crc, 14)
    header.writeUInt32LE(data.length, 18)
    header.writeUInt32LE(data.length, 22)
    header.writeUInt16LE(fileName.length, 26)
    local.push(header, fileName, data)

    const entry = Buffer.alloc(46)
    entry.writeUInt32LE(0x02014B50, 0)
    entry.writeUInt16LE(0x0314, 4)
    entry.writeUInt16LE(20, 6)
    entry.writeUInt16LE(33, 14)
    entry.writeUInt32LE(crc, 16)
    entry.writeUInt32LE(data.length, 20)
    entry.writeUInt32LE(data.length, 24)
    entry.writeUInt16LE(fileName.length, 28)
    entry.writeUInt32LE((0o100644 << 16) >>> 0, 38)
    entry.writeUInt32LE(offset, 42)
    central.push(entry, fileName)
    offset += header.length + fileName.length + data.length
  }

  const directory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054B50, 0)
  end.writeUInt16LE(Object.keys(files).length, 8)
  end.writeUInt16LE(Object.keys(files).length, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...local, directory, end])
}

// A real PNG drawn by the browser, so previews have something to show.
async function drawLogo(page: Page) {
  const data = await page.evaluate(() => {
    const canvas = document.createElement('canvas')
    canvas.width = 480
    canvas.height = 270
    const context = canvas.getContext('2d')!
    const gradient = context.createLinearGradient(0, 0, 480, 270)
    gradient.addColorStop(0, '#14365a')
    gradient.addColorStop(1, '#e2512f')
    context.fillStyle = gradient
    context.fillRect(0, 0, 480, 270)
    context.fillStyle = '#ffffff'
    context.font = 'bold 56px sans-serif'
    context.fillText('cairn', 150, 150)
    return canvas.toDataURL('image/png').split(',')[1]!
  })
  return Buffer.from(data, 'base64')
}

// Light and dark screenshots; `CAIRN_SKILL_SCREENSHOTS` collects them for review.
async function capture(page: Page, testInfo: TestInfo, name: string) {
  const directory = process.env.CAIRN_SKILL_SCREENSHOTS
  const device = testInfo.project.name.includes('webkit') ? 'phone' : 'desktop'
  if (device === 'desktop')
    await page.setViewportSize({ width: 1440, height: 900 })
  await page.evaluate(() => document.fonts.ready)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    // Let colour transitions settle after switching the scheme.
    await page.waitForTimeout(250)
    const file = `${device}-${name}-${colorScheme}.png`
    await page.screenshot({ path: directory ? path.join(directory, file) : testInfo.outputPath(file), animations: 'disabled' })
  }

  await page.emulateMedia({ colorScheme: 'light' })
}

function phone(page: Page) {
  return page.viewportSize()!.width <= 640
}

// On phones the tree and the editor share the screen; open the tree when it is hidden.
async function showTree(page: Page) {
  const back = page.getByRole('button', { name: 'Back to files', exact: true })
  if (phone(page) && await back.isVisible())
    await back.click()
}

function row(page: Page, filePath: string): Locator {
  return page.getByRole('button', { name: filePath, exact: true })
}

async function openFile(page: Page, filePath: string) {
  await showTree(page)
  await row(page, filePath).click()
}

test('builds a multi-file skill, saves one batch and round-trips it through a zip', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  await useRelayForHttpMocks(page)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const batches: {
    base: string | null
    changes: {
      op: string
      path?: string
      upload?: string
      encoding?: string
    }[]
  }[] = []
  const uploads: string[] = []
  page.on('request', (request) => {
    const pathname = new URL(request.url()).pathname
    if (request.method() === 'POST' && pathname.endsWith('/skills/global/bundle-demo/bundle'))
      batches.push(request.postDataJSON())
    if (request.method() === 'POST' && pathname.endsWith('/skills/uploads'))
      uploads.push(request.headers()['content-type'] ?? '')
  })

  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Skills library', exact: true })).toBeVisible()
  const logo = await drawLogo(page)

  // A new skill: SKILL.md, a script, an uploaded image and notes, and a nested reference.
  await page.getByRole('button', { name: 'New skill', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'New skill' })
  await page.getByLabel('Skill name').fill('bundle-demo')
  await page.getByLabel('Skill content').fill(manifest('bundle-demo', 'Check a release with a script and references'))

  await showTree(page)
  await page.getByRole('button', { name: 'New file', exact: true }).click()
  await page.getByLabel('New file path').fill('scripts/check.sh')
  await expect(page.getByLabel('Executable script')).toBeChecked()
  await page.getByRole('button', { name: 'Create file', exact: true }).click()
  await expect(page.getByRole('switch', { name: 'Executable' })).toHaveAttribute('aria-checked', 'true')
  await page.getByLabel('Supporting file content').fill('#!/usr/bin/env bash\nset -euo pipefail\n\necho "release checks passed"\n')

  await page.getByLabel('Upload skill files').setInputFiles([
    { name: 'logo.png', mimeType: 'image/png', buffer: logo },
    { name: 'notes.md', mimeType: 'text/markdown', buffer: Buffer.from('# Notes\n\nKeep the changelog short.\n') },
    { name: 'scratch.txt', mimeType: 'text/plain', buffer: Buffer.from('temporary') },
  ])
  await showTree(page)
  await expect(row(page, 'logo.png')).toBeVisible()

  await page.getByRole('button', { name: 'New folder', exact: true }).click()
  await page.getByLabel('Folder name').fill('references/deep')
  await page.getByLabel('First file name').fill('guide.md')
  await page.getByRole('button', { name: 'Create folder', exact: true }).click()
  await page.getByLabel('Supporting file content').fill('# Guide\n\nNested references stay in their folder.\n')

  // Rename with F2, delete with the Delete key: the tree is keyboard driven.
  await showTree(page)
  await row(page, 'notes.md').focus()
  await page.keyboard.press('F2')
  await page.getByLabel('New path').fill('references/notes.md')
  await page.getByRole('button', { name: 'Rename', exact: true }).click()
  await expect(row(page, 'references/notes.md')).toBeVisible()
  await row(page, 'scratch.txt').focus()
  await page.keyboard.press('Delete')
  await expect(row(page, 'scratch.txt')).toHaveCount(0)

  // Files dropped on a folder land inside it; a shebang makes them executable.
  const dropped = await page.evaluateHandle(() => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['#!/bin/sh\necho deploy\n'], 'deploy.sh', { type: 'text/x-sh' }))
    return transfer
  })
  const scripts = page.locator('[data-node-path="scripts"]')
  await scripts.dispatchEvent('dragover', { dataTransfer: dropped })
  await expect(page.getByRole('status').filter({ hasText: 'Drop into scripts/' })).toBeVisible()
  await scripts.dispatchEvent('drop', { dataTransfer: dropped })
  await expect(page.getByLabel('Supporting file content')).toHaveValue('#!/bin/sh\necho deploy\n')
  await showTree(page)
  await expect(row(page, 'scripts/deploy.sh')).toBeVisible()

  await page.getByRole('button', { name: 'Save skill', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Edit skill' }).getByText('All changes saved')).toBeVisible()
  expect(batches).toHaveLength(1)
  expect(batches[0]!.base).toBeNull()
  expect(batches[0]!.changes.map(change => change.path).sort()).toEqual(['SKILL.md', 'logo.png', 'references/deep/guide.md', 'references/notes.md', 'scripts/check.sh', 'scripts/deploy.sh'])
  // Binaries travel as raw uploads, small text inline.
  expect(batches[0]!.changes.find(change => change.path === 'logo.png')!.upload).toBeTruthy()
  expect(uploads).toEqual(['application/octet-stream'])
  await expect(dialog).toHaveCount(0)

  // Reloading shows exactly what was saved, with modes.
  await page.reload()
  await page.getByRole('button', { name: 'Edit bundle-demo', exact: true }).click()
  const editor = page.getByRole('dialog', { name: 'Edit skill' })
  await expect(editor.getByText('6 files')).toBeVisible()
  await openFile(page, 'scripts/check.sh')
  await expect(page.getByLabel('Supporting file content')).toHaveValue(/release checks passed/)
  await expect(page.getByRole('switch', { name: 'Executable' })).toHaveAttribute('aria-checked', 'true')
  await capture(page, testInfo, 'skill-workbench-script')
  await openFile(page, 'logo.png')
  const image = editor.getByRole('img', { name: 'logo.png' })
  await expect(image).toBeVisible()
  await expect.poll(() => image.evaluate(element => (element as HTMLImageElement).naturalWidth)).toBe(480)
  await expect(editor.getByText(/480 × 270 px/)).toBeVisible()
  await capture(page, testInfo, 'skill-workbench-image')

  // A second batch: move the image, drop the nested guide, clear the script's mode.
  await showTree(page)
  await row(page, 'logo.png').focus()
  await page.keyboard.press('F2')
  await page.getByLabel('New path').fill('assets/logo.png')
  await page.getByRole('button', { name: 'Rename', exact: true }).click()
  await showTree(page)
  await row(page, 'references/deep/guide.md').focus()
  await page.keyboard.press('Delete')
  await expect(editor.getByRole('button', { name: 'Restore references/deep/guide.md' })).toBeVisible()
  await openFile(page, 'scripts/check.sh')
  await page.getByRole('switch', { name: 'Executable' }).click()
  await expect(editor.getByText('3 unsaved changes')).toBeVisible()
  await page.getByRole('button', { name: 'Save skill', exact: true }).click()
  await expect(editor.getByText('All changes saved')).toBeVisible()
  expect(batches).toHaveLength(2)
  expect(batches[1]!.base).toMatch(/\w{16,}/)
  expect(batches[1]!.changes.map(change => change.op).sort()).toEqual(['delete', 'mode', 'move'])
  const saved = await workspace.api('/api/skills/global/bundle-demo/bundle')
  expect(saved.files.map((file: { path: string, executable: boolean }) => `${file.path}:${file.executable}`).sort()).toEqual([
    'SKILL.md:false',
    'assets/logo.png:false',
    'references/notes.md:false',
    'scripts/check.sh:false',
    'scripts/deploy.sh:true',
  ])

  // Export downloads the saved revision as a zip.
  const download = page.waitForEvent('download')
  await editor.getByRole('button', { name: 'Download .zip' }).click()
  const archive = await download
  expect(archive.suggestedFilename()).toBe('bundle-demo.zip')
  const zip = await readFile((await archive.path())!)
  expect(zip.subarray(0, 2).toString()).toBe('PK')
  await editor.getByRole('button', { name: 'Close', exact: true }).click()

  // Remove the skill, then import the downloaded archive back.
  await page.getByRole('button', { name: 'Delete bundle-demo', exact: true }).click()
  await page.getByRole('button', { name: 'Remove skill', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit bundle-demo', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Import', exact: true }).click()
  await page.getByLabel('Skill archive').setInputFiles({ name: 'bundle-demo.zip', mimeType: 'application/zip', buffer: zip })
  await page.getByRole('button', { name: 'Import skill', exact: true }).click()
  const imported = page.getByRole('dialog', { name: 'Edit skill' })
  await expect(imported.getByRole('heading', { name: 'bundle-demo' })).toBeVisible()
  await showTree(page)
  for (const filePath of ['SKILL.md', 'assets/logo.png', 'references/notes.md', 'scripts/check.sh', 'scripts/deploy.sh'])
    await expect(row(page, filePath)).toBeAttached()
  await imported.getByRole('button', { name: 'Close', exact: true }).click()

  // An archive with several skills asks which one to import, reusing the same upload.
  await page.getByRole('button', { name: 'Import', exact: true }).click()
  await page.getByLabel('Skill archive').setInputFiles({
    name: 'collection.zip',
    mimeType: 'application/zip',
    buffer: storedZip({
      'collection/skills/triage/SKILL.md': manifest('triage', 'Sort incoming issues'),
      'collection/skills/release-notes/SKILL.md': manifest('release-notes', 'Draft release notes'),
      'collection/skills/release-notes/templates/notes.md': '# {{version}}\n',
    }),
  })
  await page.getByRole('button', { name: 'Import skill', exact: true }).click()
  const found = page.getByRole('radiogroup', { name: 'Skill folder' })
  await expect(found.getByRole('radio')).toHaveCount(2)
  await found.getByText('release-notes', { exact: true }).click()
  await capture(page, testInfo, 'skill-import-candidates')
  await page.getByRole('button', { name: 'Import selected skill', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Edit skill' }).getByRole('heading', { name: 'release-notes' })).toBeVisible()
  expect(uploads).toHaveLength(3)
  await page.getByRole('dialog', { name: 'Edit skill' }).getByRole('button', { name: 'Close', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit release-notes', exact: true })).toBeVisible()
  expect(errors).toEqual([])
})

test('connection feedback preserves the skill footer and a manual save retry', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/reconnect-layout/bundle', 'POST', { base: null, changes: [{ op: 'write', path: 'SKILL.md', content: manifest('reconnect-layout', 'Initial') }] })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit reconnect-layout', exact: true }).click()
  const editor = page.getByLabel('Skill content')
  const submitted = manifest('reconnect-layout', 'Preserve this edit')
  await editor.fill(submitted)
  let saves = 0
  await page.route(/\/skills\/global\/reconnect-layout\/bundle$/, async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    saves++
    await route.fulfill({ status: 503, json: { error: 'Installation unavailable' } })
  })
  await page.route(/\/overview$/, route => route.fulfill({ status: 503, json: { error: 'Installation unavailable' } }))
  const dialog = page.getByRole('dialog', { name: 'Edit skill', exact: true })
  const save = dialog.getByRole('button', { name: 'Save skill', exact: true })
  await save.click()
  await expect(dialog.getByRole('status').filter({ hasText: /Connection unavailable|Connection lost/ })).toBeVisible()
  await expect(save).toBeEnabled()
  await expect(save).toBeInViewport({ ratio: 1 })
  await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeInViewport({ ratio: 1 })
  const bounds = await dialog.boundingBox()
  const button = await save.boundingBox()
  expect(button!.y + button!.height).toBeLessThanOrEqual(bounds!.y + bounds!.height)
  await expect(editor).toHaveValue(submitted)
  expect(saves).toBe(1)
  await save.click()
  await expect.poll(() => saves).toBe(2)
})

test('a stale editor reports the conflict and reloads the latest revision', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/conflict-demo/bundle', 'POST', { base: null, changes: [{ op: 'write', path: 'SKILL.md', content: manifest('conflict-demo', 'Two editors') }] })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit conflict-demo', exact: true }).click()
  await expect(page.getByLabel('Skill content')).toHaveValue(/Two editors/)

  const current = await workspace.api('/api/skills/global/conflict-demo/bundle')
  await workspace.api('/api/skills/global/conflict-demo/bundle', 'POST', { base: current.revision, changes: [{ op: 'write', path: 'NOTES.md', content: 'From elsewhere' }] })
  await page.getByLabel('Skill content').fill(manifest('conflict-demo', 'Mine'))
  await page.getByRole('button', { name: 'Save skill', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'This skill changed since you opened it.' })).toBeVisible()
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await showTree(page)
  await expect(row(page, 'NOTES.md')).toBeVisible()
  await openFile(page, 'SKILL.md')
  await expect(page.getByLabel('Skill content')).toHaveValue(/Two editors/)

  // Closing with unsaved edits asks first.
  await page.getByLabel('Skill content').fill(manifest('conflict-demo', 'Unsaved'))
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog', { name: 'Discard unsaved changes?' })).toBeVisible()
  await page.getByRole('button', { name: 'Discard changes', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
})

test('saving locks draft mutations until the batch completes', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/save-lock/bundle', 'POST', { base: null, changes: [{ op: 'write', path: 'SKILL.md', content: manifest('save-lock', 'Initial') }] })
  let release!: () => void
  const pending = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(/\/skills\/global\/save-lock\/bundle$/, async (route) => {
    if (route.request().method() === 'POST')
      await pending
    await route.continue()
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit save-lock', exact: true }).click()
  const editor = page.getByLabel('Skill content')
  const submitted = manifest('save-lock', 'Submitted')
  await editor.fill(submitted)
  await page.getByRole('button', { name: 'Save skill', exact: true }).click()
  try {
    await expect(editor).toHaveAttribute('readonly', '')
    await showTree(page)
    await expect(page.getByRole('button', { name: 'New file', exact: true })).toBeDisabled()
    await expect(page.getByRole('button', { name: 'Cancel', exact: true })).toBeDisabled()
  }
  finally {
    release()
  }

  await expect(page.getByRole('dialog', { name: 'Edit skill' }).getByText('All changes saved')).toBeVisible()
  await openFile(page, 'SKILL.md')
  await expect(editor).not.toHaveAttribute('readonly', '')
  await expect(editor).toHaveValue(submitted)
  expect((await workspace.api('/api/skills/global/save-lock/file?path=SKILL.md')).content).toBe(submitted)
})

test('file context menus rename, change modes, download and delete the selected file', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/menu-demo/bundle', 'POST', {
    base: null,
    changes: [
      { op: 'write', path: 'SKILL.md', content: manifest('menu-demo', 'Menus') },
      { op: 'write', path: 'notes.txt', content: 'Keep these bytes\n' },
    ],
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit menu-demo', exact: true }).click()
  const menu = async (filePath: string) => {
    await showTree(page)
    await row(page, filePath).hover()
    await page.getByRole('button', { name: `Actions for ${filePath}`, exact: true }).click()
    return page.getByRole('menu', { name: `Actions for ${filePath}`, exact: true })
  }

  await (await menu('notes.txt')).getByRole('menuitem', { name: /Rename or move/ }).click()
  await page.getByLabel('New path').fill('renamed.txt')
  await page.getByRole('button', { name: 'Rename', exact: true }).click()
  await (await menu('renamed.txt')).getByRole('menuitemcheckbox', { name: 'Make executable' }).click()
  await expect(row(page, 'renamed.txt').locator('..')).toContainText('+x')
  const download = page.waitForEvent('download')
  await (await menu('renamed.txt')).getByRole('menuitem', { name: 'Download file' }).click()
  expect((await download).suggestedFilename()).toBe('renamed.txt')
  await (await menu('renamed.txt')).getByRole('menuitem', { name: /Delete file/ }).click()
  await expect(row(page, 'renamed.txt')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Restore notes.txt', exact: true })).toBeVisible()
})

test('new-skill scope stays fixed while supporting files are staged', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  let release!: () => void
  const pending = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(/\/skills\/uploads$/, async (route) => {
    await pending
    await route.continue()
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'New skill', exact: true }).click()
  await page.getByLabel('Skill name').fill('scope-lock')
  await page.getByLabel('Skill content').fill(manifest('scope-lock', 'Fixed destination'))
  await page.getByLabel('Upload skill files').setInputFiles({ name: 'asset.bin', mimeType: 'application/octet-stream', buffer: Buffer.from([0, 1, 2]) })
  await page.getByRole('button', { name: 'Save skill', exact: true }).click()
  try {
    await expect(page.getByRole('dialog', { name: 'New skill' }).getByRole('combobox', { name: 'Scope', exact: true })).toBeDisabled()
    await expect(page.getByLabel('Skill name')).toBeDisabled()
  }
  finally {
    release()
  }

  await expect(page.getByRole('dialog', { name: 'Edit skill' }).getByText('All changes saved')).toBeVisible()
  expect((await workspace.api('/api/skills/global/scope-lock/bundle')).scope).toBe('global')
})

test('an archive cannot be replaced while its upload is pending', async ({ page }) => {
  await useRelayForHttpMocks(page)
  let release!: () => void
  const pending = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(/\/skills\/uploads$/, async (route) => {
    await pending
    await route.continue()
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Import', exact: true }).click()
  await page.getByLabel('Skill archive').setInputFiles({ name: 'archive-a.zip', mimeType: 'application/zip', buffer: storedZip({ 'archive-a/SKILL.md': manifest('archive-a', 'A') }) })
  await page.getByRole('button', { name: 'Import skill', exact: true }).click()
  try {
    await expect(page.getByLabel('Skill archive')).toBeDisabled()
    await expect(page.getByRole('button', { name: 'Choose another archive', exact: true })).toBeDisabled()
    await page.getByLabel('Skill archive').setInputFiles({ name: 'archive-b.zip', mimeType: 'application/zip', buffer: storedZip({ 'archive-b/SKILL.md': manifest('archive-b', 'B') }) })
    await expect(page.getByRole('dialog', { name: 'Import a skill' }).getByText('archive-a.zip', { exact: true })).toBeVisible()
  }
  finally {
    release()
  }

  await expect(page.getByRole('dialog', { name: 'Edit skill' }).getByRole('heading', { name: 'archive-a', exact: true })).toBeVisible()
})

test('plugin skill source candidates reset when its fields or the folder change', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'github',
        builtin: true,
        status: 'active',
        active: '1.0.0',
        manifest: {
          id: 'github',
          name: 'GitHub',
          version: '1.0.0',
          apiVersion: 2,
          description: 'Import skills from repositories.',
          capabilities: ['http'],
          skillSources: [{
            name: 'repository',
            title: 'GitHub repository',
            description: 'A public or private repository this account can read.',
            fields: [
              {
                key: 'repository',
                title: 'Repository',
                type: 'string',
                required: true,
                placeholder: 'owner/name or https://github.com/owner/name/tree/main/skills',
              },
              { key: 'ref', title: 'Branch, tag or commit', type: 'string' },
            ],
          }],
        },
        versions: [],
      }],
      capabilities: [],
      connections: [{
        id: 'work',
        pluginId: 'github',
        name: 'Work',
        enabled: true,
        settings: {},
        status: { state: 'ready' },
      }],
    },
  }))
  const requests: Record<string, unknown>[] = []
  await page.route(/\/skills\/import$/, async (route) => {
    requests.push(route.request().postDataJSON())
    await route.fulfill({ json: { imported: null, candidates: ['skills/first', 'skills/old-repo'] } })
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  const base = {
    source: 'plugin',
    pluginId: 'github',
    skillSource: 'repository',
    instance: 'work',
  }
  for (const [label, value, expected] of [
    ['Repository', 'bob/new-repo', { location: { repository: 'bob/new-repo' } }],
    ['Folder', 'skills/new-folder', { location: { repository: 'alice/old-repo' }, folder: 'skills/new-folder' }],
    ['Branch, tag or commit', 'develop', { location: { repository: 'alice/old-repo', ref: 'develop' } }],
  ] as const) {
    await page.getByRole('button', { name: 'Import', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: 'Import a skill', exact: true })
    await dialog.getByRole('button', { name: 'GitHub repository', exact: true }).click()
    await expect(dialog.getByRole('button', { name: 'Import skill', exact: true })).toBeDisabled()
    await dialog.getByLabel('Repository', { exact: true }).fill('alice/old-repo')
    await dialog.getByRole('button', { name: 'Import skill', exact: true }).click()
    await expect.poll(() => requests.at(-1)).toEqual({ ...base, location: { repository: 'alice/old-repo' } })
    // A candidate is a folder: choosing one submits it as the folder.
    await dialog.getByRole('radiogroup', { name: 'Skill folder', exact: true }).getByText('skills/old-repo', { exact: true }).click()
    await dialog.getByLabel(label, { exact: true }).fill(value)
    await expect(dialog.getByRole('radiogroup', { name: 'Skill folder', exact: true })).toHaveCount(0)
    await dialog.getByRole('button', { name: 'Import skill', exact: true }).click()
    await expect.poll(() => requests.at(-1)).toEqual({ ...base, ...expected })
    await dialog.getByRole('button', { name: 'Close dialog', exact: true }).click()
  }

  // A chosen candidate is sent back as the folder to import.
  await page.getByRole('button', { name: 'Import', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Import a skill', exact: true })
  await dialog.getByRole('button', { name: 'GitHub repository', exact: true }).click()
  await dialog.getByLabel('Repository', { exact: true }).fill('alice/old-repo')
  await dialog.getByRole('button', { name: 'Import skill', exact: true }).click()
  await dialog.getByRole('radiogroup', { name: 'Skill folder', exact: true }).getByText('skills/first', { exact: true }).click()
  await dialog.getByRole('button', { name: 'Import selected skill', exact: true }).click()
  await expect.poll(() => requests.at(-1)).toEqual({ ...base, location: { repository: 'alice/old-repo' }, folder: 'skills/first' })
})

test('pending replacement reads lock draft resets and source mutations', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/upload-race/bundle', 'POST', { base: null, changes: [{ op: 'write', path: 'SKILL.md', content: manifest('upload-race', 'Original') }] })
  await page.route(/\/skills\/global\/upload-race\/bundle$/, async (route) => {
    const bundle = await workspace.api('/api/skills/global/upload-race/bundle')
    await route.fulfill({
      json: {
        ...bundle,
        source: pluginSource(),
      },
    })
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit upload-race', exact: true }).click()
  await page.getByLabel('Skill content').fill(manifest('upload-race', 'Unsaved'))
  await page.evaluate(() => {
    const state = window as unknown as { skillReadStarted: boolean, releaseSkillRead: () => void }
    let release!: () => void
    const pending = new Promise<void>((resolve) => {
      release = resolve
    })
    state.skillReadStarted = false
    state.releaseSkillRead = release
    const read = File.prototype.arrayBuffer
    File.prototype.arrayBuffer = async function () {
      if (this.name === 'SKILL.md') {
        state.skillReadStarted = true
        await pending
      }

      return read.call(this)
    }
  })
  const replacement = manifest('upload-race', 'Replacement survives')
  await page.getByLabel('Upload skill files').setInputFiles({ name: 'SKILL.md', mimeType: 'text/markdown', buffer: Buffer.from(replacement) })
  try {
    await expect.poll(() => page.evaluate(() => (window as unknown as { skillReadStarted: boolean }).skillReadStarted)).toBe(true)
    await expect(page.getByRole('button', { name: 'Discard', exact: true })).toBeDisabled()
    await showTree(page)
    await page.getByRole('button', { name: 'Open imported source', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Check for updates', exact: true })).toHaveCount(0)
  }
  finally {
    await page.evaluate(() => (window as unknown as { releaseSkillRead: () => void }).releaseSkillRead())
  }

  await openFile(page, 'SKILL.md')
  await expect(page.getByLabel('Skill content')).toHaveValue(replacement)
  await expect(page.getByRole('button', { name: 'Save skill', exact: true })).toBeEnabled()
})

test('detaching a plugin source preserves unsaved files', async ({ page, workspace }) => {
  let releaseDetach!: () => void
  const detachGate = new Promise<void>((resolve) => {
    releaseDetach = resolve
  })
  let detachStarted = false
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/detach-draft/bundle', 'POST', { base: null, changes: [{ op: 'write', path: 'SKILL.md', content: manifest('detach-draft', 'Initial') }] })
  const source = pluginSource()
  let detached = false
  await page.route(/\/skills\/global\/detach-draft\/bundle$/, async (route) => {
    const bundle = await workspace.api('/api/skills/global/detach-draft/bundle')
    await route.fulfill({ json: { ...bundle, source: detached ? null : source } })
  })
  await page.route(/\/skills\/global\/detach-draft\/source$/, async (route) => {
    expect(route.request().method()).toBe('DELETE')
    detachStarted = true
    await detachGate
    detached = true
    await route.fulfill({ json: { detached: true } })
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Edit detach-draft', exact: true }).click()
  const edited = manifest('detach-draft', 'Unsaved local changes')
  await page.getByLabel('Skill content').fill(edited)
  await showTree(page)
  await page.getByRole('button', { name: 'Open imported source', exact: true }).click()
  await page.getByRole('button', { name: 'Detach source', exact: true }).click()
  await page.getByRole('button', { name: 'Confirm detach', exact: true }).click()
  try {
    await expect.poll(() => detachStarted).toBe(true)
    await expect(page.getByRole('button', { name: 'Close source', exact: true })).toBeDisabled()
    if (!phone(page))
      await expect(page.getByRole('button', { name: 'Imported source', exact: true })).toBeDisabled()
    await showTree(page)
    await row(page, 'SKILL.md').click()
    if (phone(page))
      await page.getByRole('button', { name: 'Open imported source', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Confirm detach', exact: true })).toBeVisible()
  }
  finally {
    releaseDetach()
  }

  await expect(page.getByRole('button', { name: 'Save skill', exact: true })).toBeEnabled()
  await expect(page.getByRole('dialog', { name: 'Edit skill' })).toContainText('1 unsaved change')
  await openFile(page, 'SKILL.md')
  await expect(page.getByLabel('Skill content')).toHaveValue(edited)
  expect((await workspace.api('/api/skills/global/detach-draft/file?path=SKILL.md')).content).toContain('Initial')
})

test('a source update ignores file responses from the preceding revision', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/read-race/bundle', 'POST', {
    base: null,
    changes: [
      { op: 'write', path: 'SKILL.md', content: manifest('read-race', 'Revision reads') },
      { op: 'write', path: 'notes.txt', content: 'Old notes' },
    ],
  })
  const source = pluginSource()
  const latest = 'b'.repeat(40)
  let releaseOld!: () => void
  let releaseNew!: () => void
  const oldGate = new Promise<void>((resolve) => {
    releaseOld = resolve
  })
  const newGate = new Promise<void>((resolve) => {
    releaseNew = resolve
  })
  let reads = 0
  await page.route(/\/skills\/global\/read-race\/bundle$/, async (route) => {
    await route.fulfill({ json: { ...await workspace.api('/api/skills/global/read-race/bundle'), source } })
  })
  await page.route(/\/skills\/global\/read-race\/file\?/, async (route) => {
    if (new URL(route.request().url()).searchParams.get('path') !== 'notes.txt') {
      await route.continue()
      return
    }

    const response = await workspace.api('/api/skills/global/read-race/file?path=notes.txt')
    reads += 1
    await (reads === 1 ? oldGate : newGate)
    await route.fulfill({ json: response })
  })
  await page.route(/\/skills\/global\/read-race\/source$/, route => route.fulfill({
    json: {
      source,
      latest: { version: latest, message: 'New notes', date: '2026-10-14T00:00:00Z' },
      current: false,
      changes: [{ path: 'notes.txt', status: 'modified', binary: false }],
    },
  }))
  await page.route(/\/skills\/global\/read-race\/source\/update$/, async (route) => {
    const current = await workspace.api('/api/skills/global/read-race/bundle')
    const saved = await workspace.api('/api/skills/global/read-race/bundle', 'POST', { base: current.revision, changes: [{ op: 'write', path: 'notes.txt', content: 'New notes' }] })
    await route.fulfill({ json: { ...saved, source: { ...source, version: latest } } })
  })
  try {
    await page.goto(workspacePath('/skills'))
    await authenticateWorkspace(page)
    await page.getByRole('button', { name: 'Edit read-race', exact: true }).click()
    await openFile(page, 'notes.txt')
    await expect.poll(() => reads).toBe(1)
    await showTree(page)
    await page.getByRole('button', { name: 'Open imported source', exact: true }).click()
    await page.getByRole('button', { name: 'Check for updates', exact: true }).click()
    await page.getByRole('button', { name: /^Update to/ }).click()
    await expect.poll(() => reads).toBe(2)
    const oldResponse = page.waitForResponse(response => response.url().includes('/read-race/file?path=notes.txt'))
    releaseOld()
    await (await oldResponse).finished()
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => resolve())))
    releaseNew()
    await expect(page.getByLabel('Supporting file content')).toHaveValue('New notes')
  }
  finally {
    releaseOld()
    releaseNew()
  }
})

test('members browse a skill bundle read-only', async ({ page, workspace }, testInfo) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/reader/bundle', 'POST', {
    base: null,
    changes: [
      { op: 'write', path: 'SKILL.md', content: manifest('reader', 'Read the playbook') },
      {
        op: 'write',
        path: 'scripts/check.sh',
        content: '#!/bin/sh\necho ok\n',
        executable: true,
      },
    ],
  })
  await page.route('**/api/account/session', async (route) => {
    const session = await (await route.fetch()).json()
    session.installations = session.installations.map((installation: object) => ({ ...installation, role: 'member' }))
    await route.fulfill({ json: session })
  })
  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: 'New skill', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Import', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'reader', exact: true }).click()
  const viewer = page.getByRole('dialog', { name: 'View skill' })
  await expect(viewer.getByText('Only the owner can edit skills.')).toBeVisible()
  await openFile(page, 'SKILL.md')
  await expect(viewer.getByRole('heading', { name: 'reader', level: 1 })).toBeVisible()
  await expect(page.getByRole('button', { name: 'New file', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Save skill', exact: true })).toHaveCount(0)
  await openFile(page, 'scripts/check.sh')
  await expect(page.getByLabel('Supporting file content')).toHaveAttribute('readonly', '')
  await expect(page.getByRole('switch', { name: 'Executable' })).toHaveCount(0)
  await capture(page, testInfo, 'skill-member-readonly')
})

test('reviews a source update as a diff before replacing local files', async ({ page, workspace }, testInfo) => {
  await useRelayForHttpMocks(page)
  await workspace.api('/api/skills/global/upstream/bundle', 'POST', {
    base: null,
    changes: [
      { op: 'write', path: 'SKILL.md', content: manifest('upstream', 'Imported from GitHub') },
      {
        op: 'write',
        path: 'scripts/lint.sh',
        content: '#!/bin/sh\npnpm lint\n',
        executable: true,
      },
    ],
  })
  const source = pluginSource({
    label: 'acme/agent-skills',
    location: { repository: 'acme/agent-skills', ref: 'main' },
    url: 'https://github.com/acme/agent-skills/tree/1a2b3c4d5e6f7a8b9c0d/skills/upstream',
    folder: 'skills/upstream',
    version: '1a2b3c4d5e6f7a8b9c0d',
    importedAt: Date.now() - 86400000,
    modified: true,
  })
  const latest = '9f8e7d6c5b4a39281706'
  let updated: { version: string, base: string } | undefined
  await page.route(/\/api\/skills$/, async (route) => {
    const skills = await (await route.fetch()).json()
    await route.fulfill({ json: skills.map((skill: { name: string }) => skill.name === 'upstream' ? { ...skill, source } : skill) })
  })
  await page.route(/\/skills\/global\/upstream\/bundle$/, async (route) => {
    const bundle = await (await route.fetch()).json()
    await route.fulfill({ json: { ...bundle, source } })
  })
  await page.route(/\/skills\/global\/upstream\/source$/, route => route.fulfill({
    json: {
      source,
      latest: { version: latest, message: 'Teach the skill to lint docs\n\nAlso checks links.', date: new Date().toISOString() },
      current: false,
      changes: [
        {
          path: 'SKILL.md',
          status: 'modified',
          binary: false,
          lines: [
            { kind: 'context', text: '---' },
            { kind: 'context', text: 'name: upstream' },
            { kind: 'removed', text: 'description: Imported from GitHub' },
            { kind: 'added', text: 'description: Lint code and docs before review' },
            { kind: 'context', text: '---' },
            { kind: 'gap', text: '12 unchanged lines' },
            { kind: 'context', text: '## Checks' },
            { kind: 'added', text: '- Run `scripts/lint-docs.sh` for Markdown.' },
          ],
        },
        {
          path: 'scripts/lint-docs.sh',
          status: 'added',
          binary: false,
          lines: [{ kind: 'added', text: '#!/bin/sh' }, { kind: 'added', text: 'pnpm markdownlint "**/*.md"' }],
        },
        { path: 'assets/old-diagram.png', status: 'removed', binary: true },
      ],
    },
  }))
  await page.route(/\/skills\/global\/upstream\/source\/update$/, async (route) => {
    updated = route.request().postDataJSON()
    const bundle = await workspace.api('/api/skills/global/upstream/bundle')
    await route.fulfill({ json: { ...bundle, source: { ...source, version: latest, modified: false } } })
  })

  await page.goto(workspacePath('/skills'))
  await authenticateWorkspace(page)
  await expect(page.getByTestId('skill-source')).toContainText('acme/agent-skills')
  await expect(page.getByTestId('skill-source')).toContainText('1a2b3c4')
  await capture(page, testInfo, 'skills-list')
  await page.getByRole('button', { name: 'Edit upstream', exact: true }).click()
  await showTree(page)
  await page.getByRole('button', { name: 'Open imported source', exact: true }).click()
  // The plugin's label, folder and link; no address is built on the client.
  await expect(page.getByRole('link', { name: 'acme/agent-skills' })).toHaveAttribute('href', source.url)
  await expect(page.getByText('skills/upstream', { exact: true })).toBeVisible()
  await expect(page.getByText('This skill was edited in Cairn since its last import.')).toBeVisible()
  await page.getByRole('button', { name: 'Check for updates', exact: true }).click()
  const update = page.getByRole('region', { name: 'Available update' })
  await expect(update).toContainText('Teach the skill to lint docs')
  await expect(update.getByText('description: Lint code and docs before review')).toBeVisible()
  await expect(update.getByText('12 unchanged lines')).toBeVisible()
  await update.getByText('Teach the skill to lint docs').scrollIntoViewIfNeeded()
  await update.getByText('Teach the skill to lint docs').evaluate(element => element.closest('.overflow-auto')!.scrollBy(0, element.getBoundingClientRect().top - 180))
  await capture(page, testInfo, 'skill-source-diff')
  await update.getByRole('button', { name: 'Update to 9f8e7d6', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Edits made in Cairn since the import will be lost.' })).toBeVisible()
  await update.getByRole('button', { name: 'Replace my edits', exact: true }).click()
  await expect.poll(() => updated?.version).toBe(latest)
  expect(updated!.base).toMatch(/\w{16,}/)
})
