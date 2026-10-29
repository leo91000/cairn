import Ajv from 'ajv'
import { expect, it } from 'vitest'
import { editSkillFiles } from '../../../packages/contracts/generated/editSkillFiles'
import { exportSkillArchive } from '../../../packages/contracts/generated/exportSkillArchive'
import { importSkill } from '../../../packages/contracts/generated/importSkill'
import importSchema from '../../../packages/contracts/generated/ImportSkillInput.json'
import { listSourceProjects } from '../../../packages/contracts/generated/listSourceProjects'
import bundleSchema from '../../../packages/contracts/generated/SkillBundle.json'
import changeSchema from '../../../packages/contracts/generated/SkillChange.json'
import corpus from '../../../tests/fixtures/skill-contracts.json'

it('qualifies the shared skill variants, nullable selections and file envelope', () => {
  const ajv = new Ajv({ strict: false })
  const imports = ajv.compile(importSchema)
  const changes = ajv.compile(changeSchema)
  for (const input of corpus.imports)
    expect(imports(input), JSON.stringify(imports.errors)).toBe(true)
  for (const input of corpus.invalidImports)
    expect(imports(input)).toBe(false)
  for (const input of corpus.changes)
    expect(changes(input), JSON.stringify(changes.errors)).toBe(true)
  for (const input of corpus.invalidChanges)
    expect(changes(input)).toBe(false)
  expect(ajv.compile(bundleSchema)(corpus.bundle)).toBe(true)
})

it('uses the generated resource transports with revisions, encoded scopes and paging', async () => {
  const calls: { path: string, options: RequestInit }[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push({ path, options })
    return {} as T
  }

  await importSkill(request, { source: 'archive', scope: 'global', upload: 'upload-id' })
  await editSkillFiles(request, {
    scope: 'global',
    name: 'review',
    base: 'revision',
    changes: [{ op: 'mode', path: 'check.sh', executable: true }],
  })
  await exportSkillArchive(request, {
    scope: 'global',
    name: 'review',
    offset: 512,
    revision: 'revision',
  })
  await listSourceProjects(request, {
    pluginId: 'forge',
    source: 'repositories',
    instance: 'acct 1',
    page: 2,
    query: '',
  })
  expect(calls[0]!.path).toBe('/skills/import')
  expect(JSON.parse(calls[0]!.options.body as string)).toEqual({ source: 'archive', scope: 'global', upload: 'upload-id' })
  expect(calls[1]!.path).toBe('/skills/global/review/bundle')
  expect(JSON.parse(calls[1]!.options.body as string).base).toBe('revision')
  expect(calls[2]!.path).toBe('/skills/global/review/archive?offset=512&revision=revision')
  expect(calls[3]!.path).toBe('/plugins/forge/projects?source=repositories&instance=acct+1&page=2&query=')
})
