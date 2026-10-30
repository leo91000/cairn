import type { McpInput } from '../../../packages/contracts/generated/typescript/McpInput'
import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import { beginMcpConnection } from '../../../packages/contracts/generated/beginMcpConnection'
import { cancelMcpConnection } from '../../../packages/contracts/generated/cancelMcpConnection'
import { createMcp } from '../../../packages/contracts/generated/createMcp'
import wire from '../../../packages/contracts/generated/mcp-wire.json'
import { mcpConnectionStatus } from '../../../packages/contracts/generated/mcpConnectionStatus'
import schema from '../../../packages/contracts/generated/McpInput.json'
import { updateMcp } from '../../../packages/contracts/generated/updateMcp'
import replacementSchema from '../../../packages/contracts/generated/UpdateMcpInput.json'
import corpus from '../../../tests/fixtures/mcp-contracts.json'

it('qualifies MCP settings defaults, secret changes and malformed commands from the shared corpus', () => {
  const validate = new Ajv({ useDefaults: true, strict: false }).compile(schema)
  for (const [index, input] of corpus.creates.entries()) {
    const value = structuredClone(input)
    expect(validate(value), JSON.stringify(validate.errors)).toBe(true)
    expect(value).toEqual(wire.creates[index])
  }

  for (const input of corpus.invalidCreates)
    expect(validate(structuredClone(input))).toBe(false)
})

it('generates the same revision and OAuth begin/status/cancel commands for authenticated transports', async () => {
  const calls: unknown[] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return {} as T
  }

  const connection = wire.creates[0]! as McpInput
  await createMcp(request, connection)
  await updateMcp(request, { id: 'connection', revision: 3, connection })
  await beginMcpConnection(request, { id: 'connection', native: true })
  await mcpConnectionStatus(request, { id: 'connection' })
  await cancelMcpConnection(request, { id: 'connection' })
  expect(calls).toEqual([
    ['/mcps', { method: 'POST', body: JSON.stringify(connection) }],
    ['/mcps/connection', { method: 'PUT', body: JSON.stringify({ id: 'connection', revision: 3, connection }) }],
    ['/mcps/connection/connect', { method: 'POST', body: JSON.stringify({ id: 'connection', native: true }) }],
    ['/mcps/connection/callback', { method: 'POST', body: JSON.stringify({ id: 'connection' }) }],
    ['/mcps/connection/cancel', { method: 'POST', body: JSON.stringify({ id: 'connection' }) }],
  ])
})

it('rejects replacements missing any ordinary setting instead of applying creation defaults', () => {
  const ajv = new Ajv({ useDefaults: true, strict: false })
  addFormats(ajv)
  const validate = ajv.compile(replacementSchema)
  const command = { id: '00000000-0000-4000-8000-000000000001', revision: 1, connection: structuredClone(wire.creates[0]!) }
  expect(validate(command), JSON.stringify(validate.errors)).toBe(true)
  for (const field of ['name', 'transport', 'url', 'command', 'args', 'auth', 'clientId', 'scopes', 'allowPrivateNetwork', 'enabled', 'enabledTools']) {
    const invalid = structuredClone(command)
    delete (invalid.connection as Record<string, unknown>)[field]
    expect(validate(invalid), field).toBe(false)
  }
})
