import type { AgentPluginGrants } from '../../../packages/contracts/generated/typescript/AgentPluginGrants'
import type { PluginContributionGrant } from '../../../packages/contracts/generated/typescript/PluginContributionGrant'
import type { PluginManifest } from '../../../packages/contracts/generated/typescript/PluginManifest'
import type { PluginResourceType } from '../../../packages/contracts/generated/typescript/PluginResourceType'

export type GrantDraft = Pick<AgentPluginGrants, 'grants' | 'runCredentialInstances'>
export type Contribution = PluginContributionGrant & { description: string, resourceTypes: string[] }

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
    ...(manifest.instructions
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
