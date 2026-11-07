import Ajv from 'ajv'
import { expect, it } from 'vitest'
import inputSchema from '../../../packages/contracts/generated/ListModelsInput.json'
import catalogSchema from '../../../packages/contracts/generated/ModelCatalog.json'
import corpus from '../../../tests/fixtures/model-contracts.json'

it('qualifies shared model catalogs and closed provider commands', () => {
  const ajv = new Ajv({ strict: false, useDefaults: true })
  const catalog = ajv.compile(catalogSchema)
  const input = ajv.compile(inputSchema)
  for (const value of corpus.catalogs)
    expect(catalog(structuredClone(value)), JSON.stringify(catalog.errors)).toBe(true)
  for (const value of corpus.inputs)
    expect(input(structuredClone(value)), JSON.stringify(input.errors)).toBe(true)
  for (const value of corpus.invalidInputs)
    expect(input(value)).toBe(false)
})
