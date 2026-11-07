import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import acquireSchema from '../../../packages/contracts/generated/AcquireExclusiveInput.json'
import viewSchema from '../../../packages/contracts/generated/ExclusiveReservation.json'
import statusSchema from '../../../packages/contracts/generated/ExclusiveStatusOutput.json'
import allocationSchema from '../../../packages/contracts/generated/ExecutionResources.json'
import moveSchema from '../../../packages/contracts/generated/MoveToNodeInput.json'
import moveOperationSchema from '../../../packages/contracts/generated/NodeMoveOperation.json'
import budgetSchema from '../../../packages/contracts/generated/NodeResources.json'
import corpus from '../../../tests/fixtures/placement-contracts.json'

it('qualifies placement commands and every exclusive lifecycle state against Rust schemas', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const allocation = ajv.compile(allocationSchema)
  const budget = ajv.compile(budgetSchema)
  for (const value of corpus.allocations) {
    expect(allocation(value), JSON.stringify(allocation.errors)).toBe(true)
    expect(budget(value), JSON.stringify(value)).toBe(false)
  }

  const view = ajv.compile(viewSchema)
  const status = ajv.compile(statusSchema)
  expect(status({ reservations: corpus.views }), JSON.stringify(status.errors)).toBe(true)
  expect(status(corpus.views)).toBe(false)
  expect(status({ reservations: Array.from({ length: 21 }).fill(corpus.views[0]) })).toBe(false)
  const move = ajv.compile(moveSchema)
  const operation = ajv.compile(moveOperationSchema)
  for (const value of corpus.moveOperations)
    expect(operation(value), JSON.stringify(operation.errors)).toBe(true)
  for (const value of corpus.invalidMoveOperations)
    expect(operation(value), JSON.stringify(value)).toBe(false)
  const acquire = ajv.compile(acquireSchema)
  for (const value of corpus.views)
    expect(view(value), JSON.stringify(view.errors)).toBe(true)
  for (const value of corpus.moves)
    expect(move(value), JSON.stringify(move.errors)).toBe(true)
  for (const value of corpus.invalidMoves)
    expect(move(value), JSON.stringify(value)).toBe(false)
  for (const value of corpus.acquisitions)
    expect(acquire(value), JSON.stringify(acquire.errors)).toBe(true)
  for (const value of corpus.invalidAcquisitions)
    expect(acquire(value), JSON.stringify(value)).toBe(false)
})
