import type { PluginManifest } from '../../../packages/contracts/generated/typescript/PluginManifest'
import type { GrantDraft } from '../src/plugin-grants'
import { expect, it, vi } from 'vitest'
import {
  canSelectPluginResourceRoot,
  chooseCredentialSupplier,
  contributions,
  grantDescription,
  grantKey,
  MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION,
  mergeGrantDrafts,
  removeGrant,
  removeGrantContribution,
  replaceGrantAccess,
  selectableResourceTypes,
  staleResourceRestrictions,
} from '../src/plugin-grants'
import {
  listPluginGrantResources,
  pluginResourcePathLabel,
  resolvePluginResourcePage,
  resolvePluginResourcePath,
} from '../src/plugin-resource-paths'

it('canonicalizes backend JSON fields at every grant level', () => {
  const backend: GrantDraft = JSON.parse('{"grants":[{"access":{"contributions":[{"resources":[{"id":"support","resourceType":"channel"},{"id":"general","resourceType":"channel"}],"name":"read","kind":"tool"},{"resources":[],"name":"message","kind":"hook"}],"type":"selected"},"instanceId":"one"},{"access":{"type":"full"},"instanceId":"two"}],"runCredentialInstances":["two","one"]}')
  const edited: GrantDraft = {
    grants: [
      { instanceId: 'two', access: { type: 'full' } },
      {
        instanceId: 'one',
        access: {
          type: 'selected',
          contributions: [
            { kind: 'hook', name: 'message', resources: [] },
            { kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'general' }, { resourceType: 'channel', id: 'support' }] },
          ],
        },
      },
    ],
    runCredentialInstances: ['one', 'two'],
  }
  expect(grantKey(edited)).toBe(grantKey(backend))
})

it('treats removing and reselecting backend full access without credentials as unchanged', () => {
  const base: GrantDraft = JSON.parse('{"grants":[{"access":{"type":"full"},"instanceId":"one"}],"runCredentialInstances":[]}')
  const mine = replaceGrantAccess(replaceGrantAccess(base, 'one', null), 'one', { type: 'full' })
  expect(grantKey(mine)).toBe(grantKey(base))
})

it('treats removing and reselecting a backend resource root as unchanged', () => {
  const base: GrantDraft = JSON.parse('{"grants":[{"access":{"contributions":[{"kind":"tool","name":"read","resources":[{"id":"support","resourceType":"channel"}]}],"type":"selected"},"instanceId":"one"}],"runCredentialInstances":[]}')
  const removed = replaceGrantAccess(base, 'one', { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [] }] })
  const mine = replaceGrantAccess(removed, 'one', { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }] }] })
  expect(grantKey(mine)).toBe(grantKey(base))
})

it('accepts a concurrent reduction after a semantically unchanged local full-access round trip', () => {
  const base: GrantDraft = JSON.parse('{"grants":[{"access":{"type":"full"},"instanceId":"one"}],"runCredentialInstances":[]}')
  const mine = replaceGrantAccess(replaceGrantAccess(base, 'one', null), 'one', { type: 'full' })
  const theirs: GrantDraft = JSON.parse('{"grants":[{"access":{"contributions":[{"kind":"tool","name":"read","resources":[]}],"type":"selected"},"instanceId":"one"}],"runCredentialInstances":[]}')
  expect(mergeGrantDrafts(base, mine, theirs)).toEqual(theirs)
})

it.each(['tool', 'command', 'hook'] as const)('retains each restricted type when rebasing %s roots', (kind) => {
  const support = { resourceType: 'channel', id: 'support' }
  const general = { resourceType: 'channel', id: 'general' }
  const alice = { resourceType: 'author', id: 'alice' }
  const draft = (resources: typeof support[]): GrantDraft => ({
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind, name: 'read', resources }] } }],
    runCredentialInstances: [],
  })

  const result = mergeGrantDrafts(draft([support, general, alice]), draft([support, alice]), draft([general, alice]))
  expect(result.grants[0]?.access).toEqual({
    type: 'selected',
    contributions: [{ kind, name: 'read', resources: [alice, support] }],
  })
})

it('shares four preparation slots across simultaneous resource resolutions', async () => {
  let active = 0
  let peak = 0
  let refused = 0
  const request = async <T>(path: string): Promise<T> => {
    if (active === 4) {
      refused++
      throw Object.assign(new Error('Plugin preparation is busy'), { status: 429 })
    }

    active++
    peak = Math.max(peak, active)
    try {
      await new Promise(resolve => setTimeout(resolve, 10))
      const id = new URL(path, 'https://fixture.test').searchParams.get('id')!
      return { resource: { resourceType: 'channel', id, name: id }, ancestors: [] } as T
    }
    finally {
      active--
    }
  }

  const picker = (instance: string) => Array.from({ length: 8 }, (_, index) =>
    resolvePluginResourcePath(request, 'bounded-resolution', instance, { resourceType: 'channel', id: String(index) }))

  const results = await Promise.allSettled([...picker('one'), ...picker('two')])
  expect(results.every(result => result.status === 'fulfilled')).toBe(true)
  expect(refused).toBe(0)
  expect(peak).toBe(4)
})

it('lists large pages while other pickers resolve their selected roots', async () => {
  let active = 0
  let peak = 0
  let refused = 0
  const resources = Array.from({ length: 12 }, (_, index) => ({
    resourceType: 'channel',
    id: String(index),
    name: `#${index}`,
    parent: null,
  }))
  const request = async <T>(path: string): Promise<T> => {
    if (active === 4) {
      refused++
      throw Object.assign(new Error('busy'), { status: 429 })
    }

    peak = Math.max(peak, ++active)
    try {
      await new Promise(resolve => setTimeout(resolve, 5))
      const url = new URL(path, 'https://fixture.test')
      if (!url.pathname.endsWith('/ancestors'))
        return { items: resources, nextPage: null } as T
      const id = url.searchParams.get('id')!
      return { resource: { resourceType: 'channel', id, name: `#${id}` }, ancestors: [{ name: 'Team' }] } as T
    }
    finally {
      active--
    }
  }

  const picker = async (instance: string) => {
    const page = await listPluginGrantResources(request, {
      pluginId: 'large-pages',
      instanceId: instance,
      resourceType: 'channel',
      query: '',
      page: 1,
    })
    return resolvePluginResourcePage(request, 'large-pages', instance, page.items)
  }

  const [one, two, selected] = await Promise.all([
    picker('one'),
    picker('two'),
    Promise.all(resources.slice(0, 8).map(root => resolvePluginResourcePath(request, 'large-pages', 'selected', root))),
  ])
  expect(one.map(row => row.path)).toEqual(resources.map(item => `Team › ${item.name}`))
  expect(two.map(row => row.path)).toEqual(one.map(row => row.path))
  expect(selected).toHaveLength(8)
  expect(peak).toBe(4)
  expect(refused).toBe(0)
})

it('retries busy paths, preserves successful rows and can recover the remaining path', async () => {
  vi.useFakeTimers()
  try {
    const attempts = new Map<string, number>()
    let busy = true
    const items = Array.from({ length: 8 }, (_, index) => ({
      resourceType: 'channel',
      id: String(index),
      name: `#${index}`,
      parent: null,
    }))
    const request = async <T>(path: string): Promise<T> => {
      const id = new URL(path, 'https://fixture.test').searchParams.get('id')!
      const count = (attempts.get(id) ?? 0) + 1
      attempts.set(id, count)
      if ((id === '0' && busy) || (id === '1' && count === 1))
        throw Object.assign(new Error('busy'), { status: 429 })
      return { resource: { resourceType: 'channel', id, name: `#${id}` }, ancestors: [] } as T
    }

    const loading = resolvePluginResourcePage(request, 'retry-paths', 'instance', items)
    await vi.runAllTimersAsync()
    const rows = await loading
    expect(rows.filter(row => row.path !== null)).toHaveLength(7)
    expect(rows[0]?.path).toBeNull()
    expect(attempts.get('0')).toBe(5)
    expect(attempts.get('1')).toBe(2)
    busy = false
    const recovered = await resolvePluginResourcePage(request, 'retry-paths', 'instance', items)
    expect(recovered.every(row => row.path !== null)).toBe(true)
    expect(attempts.get('0')).toBe(6)
    expect(attempts.get('2')).toBe(1)
  }
  finally {
    vi.useRealTimers()
  }
})

it('keeps independent type edits when retaining the final local root of another type', () => {
  const root = (resourceType: string, id: string) => ({ resourceType, id })
  const draft = (resources: ReturnType<typeof root>[]): GrantDraft => ({
    grants: [{ instanceId: 'instance', access: { type: 'selected', contributions: [{ kind: 'hook', name: 'message', resources }] } }],
    runCredentialInstances: [],
  })
  const result = mergeGrantDrafts(
    draft([root('channel', 'support'), root('channel', 'general'), root('author', 'alice'), root('author', 'bob')]),
    draft([root('channel', 'support'), root('author', 'alice'), root('author', 'bob')]),
    draft([root('channel', 'general'), root('author', 'alice')]),
  )
  expect(result.grants[0]?.access).toEqual({ type: 'selected', contributions: [{ kind: 'hook', name: 'message', resources: [root('author', 'alice'), root('channel', 'support')] }] })
})

it('does not retry authorization failures in resource preparation', async () => {
  let attempts = 0
  const refused = async <T>(): Promise<T> => {
    attempts++
    throw Object.assign(new Error('forbidden'), { status: 403 })
  }

  await expect(resolvePluginResourcePath(refused, 'authorization-path', 'instance', { resourceType: 'channel', id: 'denied' })).rejects.toMatchObject({ status: 403 })
  expect(attempts).toBe(1)
})

it('caps resource roots per contribution at the backend limit while allowing removal', () => {
  expect(MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION).toBe(100)
  expect(canSelectPluginResourceRoot(99, false)).toBe(true)
  expect(canSelectPluginResourceRoot(100, false)).toBe(false)
  expect(canSelectPluginResourceRoot(100, true)).toBe(true)
})

it('bounds cached resource paths while resolving grant labels', async () => {
  let requests = 0
  const request = async <T>(path: string): Promise<T> => {
    requests += 1
    const id = new URL(path, 'https://cairn.test').searchParams.get('id')!
    return {
      resource: {
        resourceType: 'channel',
        id,
        name: `#${id}`,
        parent: null,
      },
      ancestors: [],
    } as T
  }

  for (let index = 0; index <= 512; index += 1) {
    await resolvePluginResourcePath(request, 'discord', 'instance', {
      resourceType: 'channel',
      id: `channel-${index}`,
    })
  }

  await resolvePluginResourcePath(request, 'discord', 'instance', {
    resourceType: 'channel',
    id: 'channel-0',
  })
  expect(requests).toBe(514)
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

it('keeps a local no-access choice when the same grant changed remotely', () => {
  const base: GrantDraft = {
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }] }] } }],
    runCredentialInstances: [],
  }
  const mine: GrantDraft = { grants: [], runCredentialInstances: [] }
  const theirs: GrantDraft = {
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }, { resourceType: 'channel', id: 'general' }] }] } }],
    runCredentialInstances: [],
  }

  expect(mergeGrantDrafts(base, mine, theirs)).toEqual(mine)
})

it('does not turn concurrent root removals into an unrestricted resource grant', () => {
  const base: GrantDraft = {
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }, { resourceType: 'channel', id: 'general' }] }] } }],
    runCredentialInstances: [],
  }
  const mine: GrantDraft = {
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'general' }] }] } }],
    runCredentialInstances: [],
  }
  const theirs: GrantDraft = {
    grants: [{ instanceId: 'support', access: { type: 'selected', contributions: [{ kind: 'tool', name: 'read', resources: [{ resourceType: 'channel', id: 'support' }] }] } }],
    runCredentialInstances: [],
  }

  expect(mergeGrantDrafts(base, mine, theirs)).toEqual(mine)
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

it('keeps only the chosen run-credential contribution when same-package grants merge', () => {
  const credentials = { kind: 'runCredentials' as const, name: '', resources: [] }
  const messageHook = { kind: 'hook' as const, name: 'message', resources: [] }
  const readTool = { kind: 'tool' as const, name: 'read', resources: [] }
  const base: GrantDraft = { grants: [], runCredentialInstances: [] }
  const mine: GrantDraft = {
    grants: [{
      instanceId: 'automation',
      access: { type: 'selected', contributions: [credentials, messageHook] },
    }],
    runCredentialInstances: ['automation'],
  }
  const theirs: GrantDraft = {
    grants: [{
      instanceId: 'review',
      access: { type: 'selected', contributions: [credentials, readTool] },
    }],
    runCredentialInstances: ['review'],
  }

  const merged = mergeGrantDrafts(
    base,
    mine,
    theirs,
    new Map([['automation', 'grant-ui'], ['review', 'grant-ui']]),
  )

  expect(merged.runCredentialInstances).toEqual(['automation'])
  expect(merged.grants).toEqual([
    { instanceId: 'automation', access: { type: 'selected', contributions: [credentials, messageHook] } },
    { instanceId: 'review', access: { type: 'selected', contributions: [readTool] } },
  ])
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
it('preserves an explicit credential supplier when changing its selected grant to full access', () => {
  expect(replaceGrantAccess(before, 'one', { type: 'full' })).toEqual({
    grants: [
      before.grants[1],
      { instanceId: 'one', access: { type: 'full' } },
    ],
    runCredentialInstances: ['one'],
  })
  expect(replaceGrantAccess(before, 'one', null).runCredentialInstances).toEqual([])
  expect(replaceGrantAccess(before, 'one', {
    type: 'selected',
    contributions: [{ kind: 'tool', name: 'post', resources: [] }],
  }).runCredentialInstances).toEqual([])
})
it('removes one stale contribution and preserves other selected permissions and suppliers', () => {
  const after = removeGrantContribution(before, 'one', { kind: 'tool', name: 'read' })

  expect(after.grants[0]?.access).toEqual({
    type: 'selected',
    contributions: before.grants[0]!.access.type === 'selected'
      ? before.grants[0]!.access.contributions.filter(item => item.kind !== 'tool' || item.name !== 'read')
      : [],
  })
  expect(after.runCredentialInstances).toEqual(['one'])
  expect(removeGrantContribution(after, 'one', { kind: 'runCredentials', name: '' })).toEqual({
    grants: [
      {
        instanceId: 'one',
        access: {
          type: 'selected',
          contributions: [{ kind: 'tool', name: 'post', resources: [{ resourceType: 'channel', id: 'support' }] }],
        },
      },
      { instanceId: 'two', access: { type: 'full' } },
    ],
    runCredentialInstances: [],
  })
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
  expect(contributions({ ...manifest, instructions: ' \n\t' }).some(item => item.kind === 'instructions')).toBe(false)
  expect(contributions({ ...manifest, instructions: '\u0085' }).some(item => item.kind === 'instructions')).toBe(false)
  expect(contributions({ ...manifest, instructions: '\u001C' }).some(item => item.kind === 'instructions')).toBe(true)
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

it('formats nested resource ancestry from its root to the selected resource', () => {
  expect(pluginResourcePathLabel([{ name: 'Channels' }, { name: 'Team server' }], 'Support thread'))
    .toBe('Team server › Channels › Support thread')
})

it('identifies saved resource roots no longer supported by a contribution', () => {
  const resources = [
    { resourceType: 'channel', id: 'support' },
    { resourceType: 'team', id: 'engineering' },
  ]

  expect(staleResourceRestrictions(resources, ['server', 'channel'])).toEqual([
    { resourceType: 'team', id: 'engineering' },
  ])
  expect(staleResourceRestrictions(resources, [])).toEqual(resources)
})
