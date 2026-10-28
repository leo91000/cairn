import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import viewSchema from '../../../packages/contracts/generated/ExecutionNode.json'
import commandSchema from '../../../packages/contracts/generated/UpdateNodeInput.json'
import corpus from '../../../tests/fixtures/node-contracts.json'

it('qualifies current node inventory, native observation, configuration and resource bounds', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const view = ajv.compile(viewSchema)
  const command = ajv.compile(commandSchema)
  for (const value of corpus.views)
    expect(view(value), JSON.stringify(view.errors)).toBe(true)
  for (const value of corpus.commands)
    expect(command(value), JSON.stringify(command.errors)).toBe(true)
  for (const value of corpus.invalidCommands)
    expect(command(value), JSON.stringify(value)).toBe(false)
})
