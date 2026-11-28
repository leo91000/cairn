import type { ChipTone, SectionId } from './agent-access'

export const AGENT_SECTION_TITLES: Record<SectionId, string> = {
  identity: 'Identity & instructions',
  model: 'Coding agent & model',
  people: 'Who can use it',
  projects: 'Projects',
  skills: 'Skills',
  mcps: 'MCP & tools',
  plugins: 'Secrets & plugins',
  nodes: 'Nodes',
  sandbox: 'Execution mode',
  powers: 'Elevated rights',
  advanced: 'Limits',
}

export const AGENT_NAV_GROUPS: { title: string, items: SectionId[] }[] = [
  { title: 'Profile', items: ['identity', 'model'] },
  { title: 'People', items: ['people'] },
  { title: 'Resource access', items: ['projects', 'skills', 'mcps', 'plugins'] },
  { title: 'Execution', items: ['nodes', 'sandbox'] },
  { title: 'Elevated rights', items: ['powers'] },
  { title: 'Advanced', items: ['advanced'] },
]

export function formatListNames(ids: string[], name: (id: string) => string, limit = 3) {
  const listed = ids.slice(0, limit).map(name)
  return ids.length > limit ? `${listed.join(', ')} +${ids.length - limit}` : listed.join(', ')
}

export function shortCount(value: string[] | null, total: number) {
  return value === null ? 'All' : value.length ? `${value.length}/${Math.max(total, value.length)}` : 'None'
}

export function grantChip(value: string[] | null): { tone: ChipTone, label: string } {
  return value === null || value.length ? { tone: 'granted', label: 'Granted' } : { tone: 'none', label: 'None' }
}
