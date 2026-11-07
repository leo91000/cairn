import type { AgentPluginGrants } from '../../../packages/contracts/generated/typescript/AgentPluginGrants'
import type { PluginContributionGrant } from '../../../packages/contracts/generated/typescript/PluginContributionGrant'
import type { PluginInstanceGrant } from '../../../packages/contracts/generated/typescript/PluginInstanceGrant'
import type { PluginManifest } from '../../../packages/contracts/generated/typescript/PluginManifest'
import type { PluginResourceRef } from '../../../packages/contracts/generated/typescript/PluginResourceRef'
import type { PluginResourceType } from '../../../packages/contracts/generated/typescript/PluginResourceType'

export type GrantDraft = Pick<AgentPluginGrants, 'grants' | 'runCredentialInstances'>

export type Contribution = PluginContributionGrant & { description: string, resourceTypes: string[] }

export const MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION = 100

export function canSelectPluginResourceRoot(selectedCount: number, alreadySelected: boolean): boolean {
  return alreadySelected || selectedCount < MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION
}

function mergeSet<T>(base: T[], mine: T[], theirs: T[], identity: (item: T) => string): T[] {
  const items = new Map([...base, ...mine, ...theirs].map(item => [identity(item), item]))
  const baseIds = new Set(base.map(identity))
  const mineIds = new Set(mine.map(identity))
  const theirsIds = new Set(theirs.map(identity))

  return [...items.keys()]
    .filter(id => (mineIds.has(id) === baseIds.has(id) ? theirsIds : mineIds).has(id))
    .sort()
    .map(id => items.get(id)!)
}

function contributionKey(item: PluginContributionGrant): string {
  return `${item.kind}:${item.name}`
}

function contributionValueKey(item: PluginContributionGrant): string {
  return JSON.stringify({
    ...item,
    resources: [...item.resources].sort((a, b) => `${a.resourceType}:${a.id}`.localeCompare(`${b.resourceType}:${b.id}`)),
  })
}

function grantValueKey(grant: PluginInstanceGrant | undefined): string {
  return grant ? grantKey({ grants: [grant], runCredentialInstances: [] }) : ''
}

function mergeContributions(
  base: PluginContributionGrant[],
  mine: PluginContributionGrant[],
  theirs: PluginContributionGrant[],
): PluginContributionGrant[] {
  const baseByKey = new Map(base.map(item => [contributionKey(item), item]))
  const mineByKey = new Map(mine.map(item => [contributionKey(item), item]))
  const theirsByKey = new Map(theirs.map(item => [contributionKey(item), item]))
  const keys = [...new Set([...base, ...mine, ...theirs].map(contributionKey))].sort()

  return keys.flatMap((key) => {
    const baseItem = baseByKey.get(key)
    const mineItem = mineByKey.get(key)
    const theirsItem = theirsByKey.get(key)
    const baseValue = baseItem ? contributionValueKey(baseItem) : ''
    const mineValue = mineItem ? contributionValueKey(mineItem) : ''
    const theirsValue = theirsItem ? contributionValueKey(theirsItem) : ''

    if (mineValue === baseValue)
      return theirsItem ? [theirsItem] : []
    if (theirsValue === baseValue || mineValue === theirsValue)
      return mineItem ? [mineItem] : []

    // Removing a contribution is an explicit revocation. Do not let a concurrent
    // edit to its roots restore it during rebase.
    if (baseItem && (!mineItem || !theirsItem))
      return []

    const identity = (resource: PluginResourceRef) => `${resource.resourceType}:${resource.id}`
    const template = mineItem || theirsItem || baseItem
    if (!template)
      return []
    const resources = mergeSet(baseItem?.resources ?? [], mineItem?.resources ?? [], theirsItem?.resources ?? [], identity)
    if (resources.length > MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION)
      return mineItem ? [mineItem] : theirsItem ? [theirsItem] : []
    // An empty root list means unrestricted access. If both sides retained a
    // restriction but removed different roots, keep the local restriction.
    if (baseItem?.resources.length && mineItem?.resources.length && theirsItem?.resources.length && !resources.length)
      return [mineItem]
    return [{ ...template, resources }]
  })
}

function mergeGrant(
  base: PluginInstanceGrant | undefined,
  mine: PluginInstanceGrant | undefined,
  theirs: PluginInstanceGrant | undefined,
): PluginInstanceGrant | undefined {
  const baseValue = grantValueKey(base)
  const mineValue = grantValueKey(mine)
  const theirsValue = grantValueKey(theirs)
  if (mineValue === baseValue)
    return theirs
  if (theirsValue === baseValue || mineValue === theirsValue)
    return mine

  // Preserve a revocation made on either side instead of rebuilding access from
  // the surviving side's selected contributions.
  if (base && (!mine || !theirs))
    return undefined

  // Selected contributions are independent edits. Merge those by contribution and resource;
  // access-mode changes remain the local choice and are shown in the sensitive review.
  if ([base, mine, theirs].every(grant => !grant || grant.access.type === 'selected')) {
    const contributions = mergeContributions(
      base?.access.type === 'selected' ? base.access.contributions : [],
      mine?.access.type === 'selected' ? mine.access.contributions : [],
      theirs?.access.type === 'selected' ? theirs.access.contributions : [],
    )
    if (contributions.length > 500)
      return mine
    return contributions.length
      ? { instanceId: mine?.instanceId ?? theirs!.instanceId, access: { type: 'selected', contributions } }
      : undefined
  }

  return mine
}

function mergeCredentialSuppliers(
  base: string[],
  mine: string[],
  theirs: string[],
  packageByInstance: ReadonlyMap<string, string>,
): string[] {
  const packageKey = (instance: string) => packageByInstance.get(instance) ?? `unknown:${instance}`
  const packages = [...new Set([...base, ...mine, ...theirs].map(packageKey))].sort()
  const supplierFor = (instances: string[], packageId: string) => instances.find(id => packageKey(id) === packageId)

  return packages.flatMap((packageId) => {
    const baseSupplier = supplierFor(base, packageId)
    const mineSupplier = supplierFor(mine, packageId)
    const theirsSupplier = supplierFor(theirs, packageId)
    const supplier = mineSupplier === baseSupplier
      ? theirsSupplier
      : theirsSupplier === baseSupplier || mineSupplier === theirsSupplier
        ? mineSupplier
        : mineSupplier
    return supplier ? [supplier] : []
  })
}

function keepOnlySelectedCredentialContributions(
  grants: PluginInstanceGrant[],
  runCredentialInstances: string[],
  packageByInstance: ReadonlyMap<string, string>,
): PluginInstanceGrant[] {
  const packageKey = (instance: string) => packageByInstance.get(instance) ?? `unknown:${instance}`
  const supplierByPackage = new Map(runCredentialInstances.map(instance => [packageKey(instance), instance]))

  return grants.flatMap((grant) => {
    if (grant.access.type !== 'selected')
      return [grant]
    if (!grant.access.contributions.some(contribution => contribution.kind === 'runCredentials'))
      return [grant]
    if (supplierByPackage.get(packageKey(grant.instanceId)) === grant.instanceId)
      return [grant]

    const contributions = grant.access.contributions.filter(contribution => contribution.kind !== 'runCredentials')
    return contributions.length
      ? [{ ...grant, access: { type: 'selected', contributions } }]
      : []
  })
}

/** Rebase local grant edits on the latest owner-authored grant snapshot after a conflict. */
export function mergeGrantDrafts(
  base: GrantDraft,
  mine: GrantDraft,
  theirs: GrantDraft,
  packageByInstance: ReadonlyMap<string, string> = new Map(),
): GrantDraft {
  const baseByInstance = new Map(base.grants.map(grant => [grant.instanceId, grant]))
  const mineByInstance = new Map(mine.grants.map(grant => [grant.instanceId, grant]))
  const theirsByInstance = new Map(theirs.grants.map(grant => [grant.instanceId, grant]))
  const instances = [...new Set([...base.grants, ...mine.grants, ...theirs.grants].map(grant => grant.instanceId))].sort()
  const grants = instances.flatMap((instanceId) => {
    const merged = mergeGrant(baseByInstance.get(instanceId), mineByInstance.get(instanceId), theirsByInstance.get(instanceId))
    return merged ? [merged] : []
  })

  const grantsByInstance = new Map(grants.map(grant => [grant.instanceId, grant]))
  const mergedSuppliers = mergeCredentialSuppliers(
    base.runCredentialInstances,
    mine.runCredentialInstances,
    theirs.runCredentialInstances,
    packageByInstance,
  )
  const runCredentialInstances = mergedSuppliers.filter((instanceId) => {
    const grant = grantsByInstance.get(instanceId)
    return grant?.access.type === 'full'
      || (grant?.access.type === 'selected'
        && grant.access.contributions.some(contribution => contribution.kind === 'runCredentials'))
  })
  const grantsWithSelectedCredentialContributions = keepOnlySelectedCredentialContributions(
    grants,
    runCredentialInstances,
    packageByInstance,
  )

  return {
    grants: grantsWithSelectedCredentialContributions,
    runCredentialInstances,
  }
}

function isAncestorOrSelf(types: PluginResourceType[], ancestor: string, child: string): boolean {
  let current: string | null | undefined = child
  const visited = new Set<string>()
  while (current && !visited.has(current)) {
    if (current === ancestor)
      return true
    visited.add(current)
    current = types.find(type => type.name === current)?.parentType
  }

  return false
}

export function selectableResourceTypes(types: PluginResourceType[], fieldTypes: string[]): PluginResourceType[] {
  return types.filter(type => fieldTypes.some(fieldType => isAncestorOrSelf(types, type.name, fieldType)))
}

export function staleResourceRestrictions(resources: PluginResourceRef[], selectableTypes: string[]): PluginResourceRef[] {
  const selectable = new Set(selectableTypes)
  return resources.filter(resource => !selectable.has(resource.resourceType))
}

function hasNonRustWhitespace(value: string): boolean {
  return [...value].some((character) => {
    const codePoint = character.codePointAt(0)!
    return !(codePoint >= 0x09 && codePoint <= 0x0D)
      && codePoint !== 0x20
      && codePoint !== 0x85
      && codePoint !== 0xA0
      && codePoint !== 0x1680
      && !(codePoint >= 0x2000 && codePoint <= 0x200A)
      && codePoint !== 0x2028
      && codePoint !== 0x2029
      && codePoint !== 0x202F
      && codePoint !== 0x205F
      && codePoint !== 0x3000
  })
}

export function contributions(manifest: PluginManifest): Contribution[] {
  const named = (kind: PluginContributionGrant['kind'], items: {
    name: string
    description?: string
    title?: string
    resourceFields?: Array<{ resourceType: string }>
    arguments?: Array<{ resourceType?: string | null }>
  }[] = []) => items.map((item) => {
    const fieldTypes = kind === 'command'
      ? item.arguments?.flatMap(argument => argument.resourceType ? [argument.resourceType] : []) ?? []
      : item.resourceFields?.map(field => field.resourceType) ?? []
    return {
      kind,
      name: item.name,
      description: item.description || item.title || item.name,
      resources: [],
      resourceTypes: selectableResourceTypes(manifest.resources ?? [], fieldTypes).map(type => type.name),
    }
  })
  return [
    ...named('tool', manifest.tools),
    ...named('command', manifest.commands),
    ...named('hook', manifest.hooks),
    ...(manifest.runCredentials
      ? [{
          kind: 'runCredentials' as const,
          name: '',
          description: 'Run credentials — direct access as this service account',
          resources: [],
          resourceTypes: [],
        }]
      : []),
    ...(manifest.instructions && hasNonRustWhitespace(manifest.instructions)
      ? [{
          kind: 'instructions' as const,
          name: '',
          description: 'Instructions added to the agent prompt',
          resources: [],
          resourceTypes: [],
        }]
      : []),
    ...named('condition', manifest.conditions),
    ...named('projectSource', manifest.projectSources),
    ...named('skillSource', manifest.skillSources),
  ]
}

export function grantKey(draft: GrantDraft): string {
  return JSON.stringify({
    grants: draft.grants.map(grant => ({
      ...grant,
      access: grant.access.type === 'full'
        ? grant.access
        : {
            type: 'selected',
            contributions: grant.access.contributions.map(item => ({ ...item, resources: [...item.resources].sort((a, b) => `${a.resourceType}:${a.id}`.localeCompare(`${b.resourceType}:${b.id}`)) })).sort((a, b) => `${a.kind}:${a.name}`.localeCompare(`${b.kind}:${b.name}`)),
          },
    })).sort((a, b) => a.instanceId.localeCompare(b.instanceId)),
    runCredentialInstances: [...draft.runCredentialInstances].sort(),
  })
}

export function grantDescription(draft: GrantDraft, instance: string): string {
  const grant = draft.grants.find(item => item.instanceId === instance)
  if (!grant)
    return 'No access'
  if (grant.access.type === 'full')
    return 'Full access, including future contributions'
  return grant.access.contributions.map(item => `${item.kind}${item.name ? ` ${item.name}` : ''}: ${item.resources.length ? item.resources.map(resource => `${resource.resourceType} ${resource.id} and descendants`).join(', ') : 'unrestricted'}`).join('; ')
}

export function removeGrant(draft: GrantDraft, instance: string): GrantDraft {
  return { grants: draft.grants.filter(item => item.instanceId !== instance), runCredentialInstances: draft.runCredentialInstances.filter(id => id !== instance) }
}

/** Replace one instance's access while retaining credentials only if the new access allows them. */
export function replaceGrantAccess(
  draft: GrantDraft,
  instance: string,
  access: PluginInstanceGrant['access'] | null,
): GrantDraft {
  const allowsRunCredentials = access?.type === 'full'
    || (access?.type === 'selected' && access.contributions.some(item => item.kind === 'runCredentials'))
  return {
    grants: [
      ...draft.grants.filter(item => item.instanceId !== instance),
      ...(access ? [{ instanceId: instance, access }] : []),
    ],
    runCredentialInstances: allowsRunCredentials
      ? draft.runCredentialInstances
      : draft.runCredentialInstances.filter(id => id !== instance),
  }
}

/** Remove one selected contribution while preserving the instance's other grants. */
export function removeGrantContribution(
  draft: GrantDraft,
  instance: string,
  contribution: Pick<PluginContributionGrant, 'kind' | 'name'>,
): GrantDraft {
  const grants = draft.grants.flatMap((grant) => {
    if (grant.instanceId !== instance || grant.access.type !== 'selected')
      return [grant]

    const contributions = grant.access.contributions.filter(item => item.kind !== contribution.kind || item.name !== contribution.name)
    return contributions.length ? [{ ...grant, access: { type: 'selected' as const, contributions } }] : []
  })
  const runCredentialInstances = contribution.kind === 'runCredentials'
    ? draft.runCredentialInstances.filter(id => id !== instance)
    : draft.runCredentialInstances

  return { grants, runCredentialInstances }
}

/** Moving the supplier also withdraws a selected credentials contribution on the previous instance. */
export function chooseCredentialSupplier(draft: GrantDraft, instances: string[], supplier: string | null): GrantDraft {
  return {
    grants: draft.grants.flatMap((grant) => {
      if (!instances.includes(grant.instanceId) || grant.instanceId === supplier || grant.access.type === 'full')
        return [grant]
      const contributions = grant.access.contributions.filter(item => item.kind !== 'runCredentials')
      return contributions.length ? [{ ...grant, access: { type: 'selected' as const, contributions } }] : []
    }),
    runCredentialInstances: [...draft.runCredentialInstances.filter(id => !instances.includes(id)), ...(supplier ? [supplier] : [])],
  }
}
