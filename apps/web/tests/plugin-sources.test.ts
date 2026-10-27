import type { PluginProject } from '../../../packages/contracts/contracts'
import type { Catalog, PluginConnection, PluginEntry } from '../src/plugins'
import { expect, it } from 'vitest'
import {
  pluginSources,
  projectMatches,
  projectsPath,
  updatedLabel,
} from '../src/plugin-sources'

function entry(id: string, manifest: Partial<NonNullable<PluginEntry['manifest']>>, status: PluginEntry['status'] = 'active'): PluginEntry {
  return {
    id,
    builtin: true,
    status,
    active: '1.0.0',
    versions: [],
    setup: null,
    manifest: {
      id,
      name: id[0]!.toUpperCase() + id.slice(1),
      version: '1.0.0',
      description: '',
      apiVersion: 2,
      capabilities: [],
      ...manifest,
    },
  }
}

function connection(id: string, pluginId: string, enabled = true) {
  return {
    id,
    pluginId,
    name: id,
    enabled,
  } as PluginConnection
}

const repository = {
  name: 'repository',
  title: 'GitHub repository',
  fields: [{
    key: 'repository',
    title: 'Repository',
    type: 'string' as const,
    required: true,
  }],
}
const catalog: Pick<Catalog, 'packages' | 'connections'> = {
  packages: [
    entry('github', { projectSources: [{ name: 'repositories', title: 'GitHub repository', description: 'Repositories this account reaches.' }], skillSources: [repository] }),
    entry('gitlab', { projectSources: [{ name: 'projects', title: 'GitLab project' }] }),
    entry('review', { projectSources: [{ name: 'projects', title: 'Unreviewed' }] }, 'needs_approval'),
    entry('discord', {}),
  ],
  connections: [
    connection('work', 'github'),
    connection('paused', 'github', false),
    connection('personal', 'github'),
    connection('old', 'gitlab', false),
    connection('waiting', 'review'),
    connection('bot', 'discord'),
  ],
}

it('offers the sources of approved packages with an enabled instance to import through', () => {
  const projects = pluginSources(catalog, 'projectSources')
  // GitLab has only a paused instance; the unreviewed package cannot run.
  expect(projects.map(source => source.key)).toEqual(['github:repositories'])
  expect(projects[0]).toMatchObject({
    pluginId: 'github',
    source: 'repositories',
    title: 'GitHub repository',
    description: 'Repositories this account reaches.',
    pluginName: 'Github',
    fields: [],
  })
  expect(projects[0]!.instances.map(instance => instance.id)).toEqual(['work', 'personal'])

  const skills = pluginSources(catalog, 'skillSources')
  expect(skills.map(source => [source.key, source.fields.map(field => field.key)])).toEqual([['github:repository', ['repository']]])
  expect(pluginSources({ packages: catalog.packages, connections: [] }, 'skillSources')).toEqual([])
})

it('pages a source through one instance, and filters what it loaded on every typed word', () => {
  expect(projectsPath('github', 'repositories', 'work', 2)).toBe('/plugins/github/projects?source=repositories&instance=work&page=2')
  const project: PluginProject = {
    id: 'leo91000/cairn',
    name: 'leo91000/cairn',
    description: 'A private control room',
    defaultBranch: 'main',
    badges: ['Private', 'Rust'],
    imported: false,
  }
  expect(projectMatches(project, '')).toBe(true)
  expect(projectMatches(project, 'cairn rust')).toBe(true)
  expect(projectMatches(project, 'CONTRÔLE')).toBe(false)
  expect(projectMatches(project, 'control private')).toBe(true)
  expect(projectMatches(project, 'kotlin')).toBe(false)
})

it('says when a project was last updated, and nothing without a valid date', () => {
  const now = Date.parse('2026-10-20T12:00:00Z')
  expect(updatedLabel('2026-10-17T12:00:00Z', now)).toBe('Updated 3 days ago')
  expect(updatedLabel('2026-10-20T11:59:30Z', now)).toBe('Updated just now')
  expect(updatedLabel(null, now)).toBe('')
  expect(updatedLabel('soon', now)).toBe('')
})
