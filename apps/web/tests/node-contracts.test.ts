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

it('keeps every node count and timestamp within the exact cross-language integer range', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const validate = ajv.compile(viewSchema)
  for (const path of corpus.wireNumberPaths) {
    for (const number of [0, Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER + 1, -1]) {
      const view = structuredClone(corpus.views[0])
      const parts = path.slice(1).split('/')
      const field = parts.pop()!
      const object = parts.reduce<any>((value, key) => value[key], view)
      object[field] = number
      expect(validate(view), `${path}: ${number}`).toBe(number >= 0 && Number.isSafeInteger(number))
    }
  }
})
