import type { PluginManifest } from '../../../packages/contracts/generated/typescript/PluginManifest'
import type { GrantDraft } from '../src/plugin-grants'
import { expect, it } from 'vitest'
import {
  chooseCredentialSupplier,
  contributions,
  grantDescription,
  grantKey,
  removeGrant,
  selectableResourceTypes,
} from '../src/plugin-grants'

const before: GrantDraft = {
  grants: [
    { instanceId: 'one', access: { type: 'selected', contributions: [{ kind: 'runCredentials', name: '', resources: [] }, { kind: 'tool', name: 'post', resources: [{ resourceType: 'channel', id: 'support' }] }] } },
    { instanceId: 'two', access: { type: 'full' } },
  ],
  runCredentialInstances: ['one'],
}

it('moving credentials withdraws only the old selected credential contribution', () => {
  const after = chooseCredentialSupplier(before, ['one', 'two'], 'two')
  expect(after.runCredentialInstances).toEqual(['two'])
  expect(after.grants[0]!.access).toEqual({ type: 'selected', contributions: [{ kind: 'tool', name: 'post', resources: [{ resourceType: 'channel', id: 'support' }] }] })
  expect(after.grants[1]).toEqual(before.grants[1])
  expect(before.runCredentialInstances).toEqual(['one'])
})
it('removing a supplier never chooses another full instance', () => {
  expect(removeGrant(before, 'one')).toEqual({ grants: [before.grants[1]], runCredentialInstances: [] })
  expect(chooseCredentialSupplier(before, ['one', 'two'], null).runCredentialInstances).toEqual([])
})
it('does not keep an empty selected grant after moving its only contribution', () => {
  const draft: GrantDraft = { grants: [{ instanceId: 'one', access: { type: 'selected', contributions: [{ kind: 'runCredentials', name: '', resources: [] }] } }, before.grants[1]!], runCredentialInstances: ['one'] }
  expect(chooseCredentialSupplier(draft, ['one', 'two'], 'two').grants).toEqual([before.grants[1]])
})
it('compares permissions irrespective of ordering and describes restrictions for review', () => {
  expect(grantKey({ ...before, grants: [...before.grants].reverse() })).toBe(grantKey(before))
  expect(grantDescription(before, 'one')).toContain('channel support and descendants')
  expect(grantKey(removeGrant(before, 'one'))).not.toBe(grantKey(before))
})
it('offers every declared contribution and no credentials for packages without them', () => {
  const manifest = {
    tools: [{ name: 'read', description: 'Read' }],
    commands: [{ name: 'post' }],
    hooks: [{ name: 'message' }],
    instructions: 'Guide',
    runCredentials: {},
    conditions: [{ name: 'done' }],
    projectSources: [{ name: 'repos', title: 'Repositories' }],
    skillSources: [{ name: 'skills' }],
  } as PluginManifest
  expect(contributions(manifest).map(item => item.kind)).toEqual(['tool', 'command', 'hook', 'runCredentials', 'instructions', 'condition', 'projectSource', 'skillSource'])
  expect(contributions({ ...manifest, runCredentials: null }).some(item => item.kind === 'runCredentials')).toBe(false)
})
it('offers only resource types that can restrict a declared tool, command or hook field', () => {
  const types = [
    { name: 'server', description: 'Servers' },
    { name: 'channel', description: 'Channels', parentType: 'server' },
    { name: 'thread', description: 'Threads', parentType: 'channel' },
    { name: 'author', description: 'Authors' },
  ]
  const manifest = {
    resources: types,
    tools: [{ name: 'read', resourceFields: [{ path: '/channel', resourceType: 'channel' }] }],
    commands: [{ name: 'post', arguments: [{ name: 'channel', resourceType: 'channel' }] }],
    hooks: [{ name: 'message', resourceFields: [{ path: '/channel', resourceType: 'channel' }] }],
  } as PluginManifest

  expect(contributions(manifest).filter(item => item.kind !== 'runCredentials').map(item => item.resourceTypes)).toEqual([
    ['server', 'channel'],
    ['server', 'channel'],
    ['server', 'channel'],
  ])
  expect(selectableResourceTypes(types, ['thread']).map(type => type.name)).toEqual(['server', 'channel', 'thread'])
  expect(selectableResourceTypes(types, []).map(type => type.name)).toEqual([])
})
