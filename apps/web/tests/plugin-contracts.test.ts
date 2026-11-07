import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { managePlugin } from '../../../packages/contracts/generated/managePlugin'
import inputSchema from '../../../packages/contracts/generated/ManagePluginInput.json'
import outputSchema from '../../../packages/contracts/generated/ManagePluginOutput.json'
import wire from '../../../packages/contracts/generated/plugin-wire.json'
import corpus from '../../../tests/fixtures/plugin-contracts.json'

it('qualifies typed plugin envelopes without losing manifest-defined JSON', () => {
  const ajv = new Ajv({ strict: false })
  addFormats(ajv)
  const input = ajv.compile(inputSchema)
  const output = ajv.compile(outputSchema)
  for (const value of [...corpus.inputs, ...wire.inputs])
    expect(input(value), JSON.stringify(input.errors)).toBe(true)
  for (const value of corpus.invalidInputs)
    expect(input(value), JSON.stringify(value)).toBe(false)
  for (const value of wire.outputs) {
    expect(output(value), JSON.stringify(output.errors)).toBe(true)
    expect(JSON.stringify(value)).not.toMatch(/nativeAuthority|grantedAccess|ownerAccessId|synthetic-secret/)
  }
})

it('sends a closed plugin command through the generated client', async () => {
  const calls: unknown[] = []
  await managePlugin(async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return wire.outputs[0] as T
  }, { kind: 'setup', pluginId: 'fixture' })
  expect(calls).toEqual([['/plugins/manage', { method: 'POST', body: JSON.stringify({ kind: 'setup', pluginId: 'fixture' }) }]])
})
