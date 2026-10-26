import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { createProject } from '../../../packages/contracts/generated/createProject'
import createSchema from '../../../packages/contracts/generated/CreateProjectInput.json'
import { deleteProject } from '../../../packages/contracts/generated/deleteProject'
import { getProject } from '../../../packages/contracts/generated/getProject'
import { listProjects } from '../../../packages/contracts/generated/listProjects'
import wire from '../../../packages/contracts/generated/project-wire.json'
import projectSchema from '../../../packages/contracts/generated/Project.json'
import { replaceProject } from '../../../packages/contracts/generated/replaceProject'
import corpus from '../../../tests/fixtures/project-contracts.json'

it('qualifies the generated project settings defaults, source metadata and wire shape', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const create = ajv.compile(createSchema)
  const project = ajv.compile(projectSchema)
  for (const [index, input] of corpus.creates.entries()) {
    const value = structuredClone(input)
    expect(create(value), JSON.stringify(create.errors)).toBe(true)
    expect(value).toEqual(wire.creates[index])
  }

  for (const value of wire.views) {
    expect(project(value), JSON.stringify(project.errors)).toBe(true)
    expect(JSON.parse(JSON.stringify(value))).toEqual(value)
  }

  // UUID version is a server rule in addition to JSON Schema's UUID format.
  for (const [index, value] of corpus.invalidCreates.entries()) {
    if (index === 1)
      continue
    expect(create(structuredClone(value)), JSON.stringify(value)).toBe(false)
  }
})

it('uses generated project calls with complete settings and the expected revision', async () => {
  const calls: unknown[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return wire.views[0] as T
  }

  const settings = wire.creates[0].settings
  const projectId = wire.views[0].id
  const revision = wire.views[0].revision
  await createProject(request, { settings })
  await replaceProject(request, { projectId, revision, settings })
  await getProject(request, { projectId })
  await listProjects(request, {})
  await deleteProject(request, { projectId })
  expect(calls).toEqual([
    ['/projects', { method: 'POST', body: JSON.stringify({ settings }) }],
    [`/projects/${projectId}`, { method: 'PUT', body: JSON.stringify({ projectId, revision, settings }) }],
    [`/projects/${projectId}`, { method: 'GET' }],
    ['/projects', { method: 'GET' }],
    [`/projects/${projectId}`, { method: 'DELETE' }],
  ])
})
