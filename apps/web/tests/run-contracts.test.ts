import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { cancelRun } from '../../../packages/contracts/generated/cancelRun'
import { cleanupRun } from '../../../packages/contracts/generated/cleanupRun'
import listSchema from '../../../packages/contracts/generated/ListRunsInput.json'
import retrySchema from '../../../packages/contracts/generated/Retry.json'
import { retryRun } from '../../../packages/contracts/generated/retryRun'
import wire from '../../../packages/contracts/generated/run-wire.json'
import inputSchema from '../../../packages/contracts/generated/RunInput.json'
import { runMission } from '../../../packages/contracts/generated/runMission'
import stateSchema from '../../../packages/contracts/generated/RunState.json'
import corpus from '../../../tests/fixtures/run-contracts.json'

it('qualifies run commands, list defaults, lifecycle states and retry wire values', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const command = ajv.compile(inputSchema)
  const list = ajv.compile(listSchema)
  const state = ajv.compile(stateSchema)
  const retry = ajv.compile(retrySchema)
  for (const value of corpus.commands)
    expect(command(structuredClone(value)), JSON.stringify(command.errors)).toBe(true)
  for (const value of corpus.invalidCommands)
    expect(command(structuredClone(value))).toBe(false)
  for (const [index, value] of corpus.lists.entries()) {
    const actual = structuredClone(value)
    expect(list(actual), JSON.stringify(list.errors)).toBe(true)
    expect(actual).toEqual(wire.lists[index])
  }

  for (const value of corpus.invalidLists)
    expect(list(structuredClone(value))).toBe(false)
  for (const value of wire.states)
    expect(state(value), JSON.stringify(state.errors)).toBe(true)
  for (const value of wire.retries)
    expect(retry(value), JSON.stringify(retry.errors)).toBe(true)
})

it('uses distinct generated calls for admission, retry, cancel and cleanup', async () => {
  const calls: unknown[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return {} as T
  }

  const runId = corpus.commands[0].runId
  await runMission(request, { taskId: runId })
  await retryRun(request, { runId })
  await cancelRun(request, { runId })
  await cleanupRun(request, { runId })
  expect(calls).toEqual([
    [`/tasks/${runId}/run`, { method: 'POST', body: JSON.stringify({ taskId: runId }) }],
    ...['retry', 'cancel', 'cleanup'].map(action => [`/runs/${runId}/${action}`, { method: 'POST', body: JSON.stringify({ runId }) }]),
  ])
})
