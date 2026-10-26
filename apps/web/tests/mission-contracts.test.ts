import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { createMission } from '../../../packages/contracts/generated/createMission'
import createSchema from '../../../packages/contracts/generated/CreateMissionInput.json'
import { deleteMission } from '../../../packages/contracts/generated/deleteMission'
import { getMission } from '../../../packages/contracts/generated/getMission'
import { listMissions } from '../../../packages/contracts/generated/listMissions'
import wire from '../../../packages/contracts/generated/mission-wire.json'
import missionSchema from '../../../packages/contracts/generated/Mission.json'
import { previewSchedule } from '../../../packages/contracts/generated/previewSchedule'
import { replaceMission } from '../../../packages/contracts/generated/replaceMission'
import { setMissionState } from '../../../packages/contracts/generated/setMissionState'
import corpus from '../../../tests/fixtures/mission-contracts.json'

it('qualifies mission defaults, nullable selections, bounds and Rust wire values', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const create = ajv.compile(createSchema)
  const view = ajv.compile(missionSchema)
  for (const [index, input] of corpus.creates.entries()) {
    const value = structuredClone(input)
    expect(create(value), JSON.stringify(create.errors)).toBe(true)
    expect(value).toEqual(wire.creates[index])
  }

  for (const input of corpus.invalidCreates)
    expect(create(structuredClone(input)), JSON.stringify(input)).toBe(false)
  for (const mission of wire.views)
    expect(view(mission), JSON.stringify(view.errors)).toBe(true)
})

it('uses generated commands for complete edits, restrictive state, deletion and cron preview', async () => {
  const calls: unknown[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return wire.views[0] as T
  }

  const settings = wire.creates[0].settings
  const taskId = wire.views[0].id
  const revision = wire.views[0].revision
  await createMission(request, { settings })
  await replaceMission(request, { taskId, revision, settings })
  await setMissionState(request, {
    taskId,
    revision,
    enabled: false,
    archived: true,
  })
  await getMission(request, { taskId })
  await listMissions(request, {})
  await deleteMission(request, { taskId })
  await previewSchedule(request, { cron: '0 9 * * *', timezone: 'UTC' })
  expect(calls).toEqual([
    ['/tasks', { method: 'POST', body: JSON.stringify({ settings }) }],
    [`/tasks/${taskId}`, { method: 'PUT', body: JSON.stringify({ taskId, revision, settings }) }],
    [`/tasks/${taskId}/state`, {
      method: 'POST',
      body: JSON.stringify({
        taskId,
        revision,
        enabled: false,
        archived: true,
      }),
    }],
    [`/tasks/${taskId}`, { method: 'GET' }],
    ['/tasks', { method: 'GET' }],
    [`/tasks/${taskId}`, { method: 'DELETE' }],
    ['/schedule/preview', { method: 'POST', body: JSON.stringify({ cron: '0 9 * * *', timezone: 'UTC' }) }],
  ])
})
