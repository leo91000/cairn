import {
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { choicePageStatus, pluginFormLookup } from '../src/plugin-form-lookups'

const { operation } = vi.hoisted(() => ({ operation: vi.fn() }))
vi.mock('../src/plugin-operations', () => ({ pluginOperation: operation }))

beforeEach(() => operation.mockReset())

describe('bound remote choices', () => {
  it('keeps current scope and secrets with the editor and demands the host lookup echo', async () => {
    const body = {
      revision: 'r1',
      parentId: 'T1',
      secrets: { token: 'synthetic-secret' },
      settings: { channel: 'C1' },
    }
    const bound = pluginFormLookup(() => '/form', () => body)
    const lookup = { field: 'channel', query: 'name', cursor: 'opaque' }
    operation.mockResolvedValue({ lookup, options: { channel: [] }, pages: { channel: { nextCursor: 'another' } } })
    const result = await bound.request(lookup)
    expect(operation).toHaveBeenCalledWith('/form', { ...body, lookup })
    expect(result.pages?.channel.nextCursor).toBe('another')
    expect(result.values).toEqual({})
    for (const response of [null, {}, { lookup: { ...lookup, query: 'other' } }, { lookup: { ...lookup, cursor: undefined } }]) {
      operation.mockResolvedValue(response)
      await expect(bound.request(lookup)).rejects.toThrow('did not confirm')
    }
  })

  it('rejects an answer after edits, parent/revision changes, or an editor switch', async () => {
    for (const change of ['value', 'secret', 'parent', 'revision', 'path', 'package']) {
      let path = '/instance/one/form'
      let version = '1'
      const body = {
        revision: 'r1',
        parentId: 'T1',
        settings: { channel: 'C1' },
        secrets: { token: 'old' },
      }
      const bound = pluginFormLookup(() => path, () => body, () => version)
      const lookup = { field: 'channel', query: '' }
      let release!: (value: unknown) => void
      operation.mockReturnValue(new Promise(resolve => release = resolve))
      const pending = bound.request(lookup)
      if (change === 'value')
        body.settings.channel = 'C2'
      if (change === 'secret')
        body.secrets.token = 'new'
      if (change === 'parent')
        body.parentId = 'T2'
      if (change === 'revision')
        body.revision = 'r2'
      if (change === 'path')
        path = '/instance/two/form'
      if (change === 'package')
        version = '2'
      release({ lookup })
      await expect(pending).rejects.toThrow('form changed')
    }
  })

  it('retains names only while the rest of the form scope is unchanged', () => {
    const body = { settings: { channel: 'C1', region: 'eu' }, token: 'synthetic-one' }
    const bound = pluginFormLookup(() => '/form', () => body)
    const before = JSON.stringify(bound.context('channel'))
    body.settings.channel = 'C2'
    expect(JSON.stringify(bound.context('channel'))).toBe(before)
    body.settings.region = 'us'
    expect(JSON.stringify(bound.context('channel'))).not.toBe(before)
    body.settings.region = 'eu'
    body.token = 'synthetic-two'
    expect(JSON.stringify(bound.context('channel'))).not.toBe(before)
  })

  it('does not infer completeness from a short or empty page', () => {
    expect(choicePageStatus(0, 'next')).toBe('0 choices on this page · More available')
    expect(choicePageStatus(1, null)).toBe('1 choice on this page · End of results')
    expect(choicePageStatus(200, undefined)).toBe('200 choices on this page · Completeness unknown')
  })
})
