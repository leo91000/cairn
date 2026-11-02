import type { PluginManifest } from '../../../packages/contracts/generated/typescript/PluginManifest'
import type { GrantDraft } from '../src/plugin-grants'
import { expect, it } from 'vitest'
import {
  canSelectPluginResourceRoot,
  chooseCredentialSupplier,
  contributions,
  grantDescription,
  grantKey,
  MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION,
  mergeGrantDrafts,
  removeGrant,
  selectableResourceTypes,
} from '../src/plugin-grants'

it('caps resource roots per contribution at the backend limit while allowing removal', () => {
  expect(MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION).toBe(100)
  expect(canSelectPluginResourceRoot(99, false)).toBe(true)
  expect(canSelectPluginResourceRoot(100, false)).toBe(false)
  expect(canSelectPluginResourceRoot(100, true)).toBe(true)
})

it('rebases concurrent grant additions and independent resource roots', () => {
  const base: GrantDraft = {
    grants: [{
      instanceId: 'support',
      access: {
        type: 'selected',
        contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }] }],
      },
    }],
    runCredentialInstances: [],
  }
  const mine: GrantDraft = {
    grants: [
      {
        instanceId: 'support',
        access: {
          type: 'selected',
          contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }, { resourceType: 'channel', id: 'general' }] }],
        },
      },
      { instanceId: 'automation', access: { type: 'full' } },
    ],
    runCredentialInstances: ['automation'],
  }
  const theirs: GrantDraft = {
    grants: [
      {
        instanceId: 'support',
        access: {
          type: 'selected',
          contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }, { resourceType: 'server', id: 'team' }] }],
        },
      },
      {
        instanceId: 'review',
        access: {
          type: 'selected',
          contributions: [
            { kind: 'hook', name: 'message', resources: [] },
            { kind: 'runCredentials', name: '', resources: [] },
          ],
        },
      },
    ],
    runCredentialInstances: ['review'],
  }

  const merged = mergeGrantDrafts(base, mine, theirs, new Map([['automation', 'automation-package'], ['review', 'review-package']]))

  expect(merged.grants.find(grant => grant.instanceId === 'support')?.access).toEqual({
    type: 'selected',
    contributions: [{
      kind: 'tool',
      name: 'read',
      resources: [
        { resourceType: 'channel', id: 'general' },
        { resourceType: 'channel', id: 'support' },
        { resourceType: 'server', id: 'team' },
      ],
    }],
  })
  expect(merged.grants.map(grant => grant.instanceId)).toEqual(['automation', 'review', 'support'])
  expect(merged.runCredentialInstances).toEqual(['automation', 'review'])
})

it('keeps credential suppliers for unknown instances distinct while rebasing', () => {
  const merged = mergeGrantDrafts(
    {
      grants: [
        { instanceId: 'support', access: { type: 'full' } },
        { instanceId: 'automation', access: { type: 'full' } },
      ],
      runCredentialInstances: [],
    },
    {
      grants: [
        { instanceId: 'support', access: { type: 'full' } },
        { instanceId: 'automation', access: { type: 'full' } },
      ],
      runCredentialInstances: ['support'],
    },
    {
      grants: [
        { instanceId: 'support', access: { type: 'full' } },
        { instanceId: 'automation', access: { type: 'full' } },
      ],
      runCredentialInstances: ['automation'],
    },
  )

  expect(merged.runCredentialInstances).toEqual(['automation', 'support'])
})

it('rebases same-package suppliers to the local choice when a new instance is in the catalog', () => {
  const base: GrantDraft = {
    grants: [
      { instanceId: 'support', access: { type: 'full' } },
    ],
    runCredentialInstances: ['support'],
  }
  const mine: GrantDraft = {
    grants: [
      { instanceId: 'support', access: { type: 'full' } },
      { instanceId: 'automation', access: { type: 'full' } },
    ],
    runCredentialInstances: ['automation'],
  }
  const theirs: GrantDraft = {
    grants: [
      { instanceId: 'support', access: { type: 'full' } },
      { instanceId: 'review', access: { type: 'full' } },
    ],
    runCredentialInstances: ['review'],
  }

  const merged = mergeGrantDrafts(
    base,
    mine,
    theirs,
    new Map([
      ['support', 'grant-ui'],
      ['automation', 'grant-ui'],
      ['review', 'grant-ui'],
    ]),
  )

  expect(merged.runCredentialInstances).toEqual(['automation'])
})

it('drops a locally selected supplier when a concurrent edit revokes its grant', () => {
  const base: GrantDraft = {
    grants: [{ instanceId: 'automation', access: { type: 'full' } }],
    runCredentialInstances: [],
  }
  const mine: GrantDraft = { ...base, runCredentialInstances: ['automation'] }
  const theirs: GrantDraft = { grants: [], runCredentialInstances: [] }

  expect(mergeGrantDrafts(base, mine, theirs)).toEqual(theirs)
})

it('drops a supplier when a concurrent edit removes its run-credentials permission', () => {
  const base: GrantDraft = {
    grants: [{ instanceId: 'automation', access: { type: 'full' } }],
    runCredentialInstances: [],
  }
  const mine: GrantDraft = { ...base, runCredentialInstances: ['automation'] }
  const theirs: GrantDraft = {
    grants: [{
      instanceId: 'automation',
      access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [] }] },
    }],
    runCredentialInstances: [],
  }

  expect(mergeGrantDrafts(base, mine, theirs)).toEqual(theirs)
})

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
