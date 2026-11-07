import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import installSchema from '../../../packages/contracts/generated/InstallPluginPackageInput.json'
import operationSchema from '../../../packages/contracts/generated/PluginOperation.json'
import programSchema from '../../../packages/contracts/generated/PluginProgramInput.json'
import corpus from '../../../tests/fixtures/plugin-package-contracts.json'

it('qualifies package commands and async results against the Rust-owned schemas', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  for (const [schema, valid, invalid] of [
    [installSchema, corpus.installs, corpus.invalidInstalls],
    [programSchema, corpus.programs, corpus.invalidPrograms],
    [operationSchema, corpus.operations, []],
  ] as const) {
    const validate = ajv.compile(schema)
    for (const value of valid)
      expect(validate(value), JSON.stringify(validate.errors)).toBe(true)
    for (const value of invalid)
      expect(validate(value), JSON.stringify(value)).toBe(false)
  }
})
