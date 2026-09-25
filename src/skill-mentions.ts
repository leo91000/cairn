import type { Agent, Skill } from '../shared/contracts'

// Mirrors `skills::mentions` on the server: `$name` after a boundary, outside code.
const MENTION = /(?<![\w$\\])\$([a-z0-9][a-z0-9-]{0,63})(?![\w-])/g
const CODE = /```[\s\S]*?```|`[^`\n]*`/g
const TYPING = /(?<![\w$\\])\$([a-z0-9-]*)$/

export interface SkillOption { name: string, description: string, scope: string }
export interface Mention { start: number, end: number, query: string }
export interface MentionSegment { text: string, skill?: string }

// Chats receive every valid skill the agent may use in the chat's projects.
export function chatSkills(skills: Skill[], agent: Agent | undefined, projectId: string | null | undefined): SkillOption[] {
  if (!agent)
    return []
  const projects = agent.access.projects
  const inScope = (scope: string) => scope === 'global' || (projectId ? scope === projectId : projects === null || projects.includes(scope))
  const byName = new Map<string, SkillOption>()
  for (const skill of skills) {
    if (!skill.valid || !inScope(skill.scope) || (agent.access.skills !== null && !agent.access.skills.includes(`${skill.scope}/${skill.name}`)))
      continue
    // A project skill shadows the global one with the same name in the list.
    if (!byName.has(skill.name) || skill.scope !== 'global')
      byName.set(skill.name, { name: skill.name, description: skill.description, scope: skill.scope })
  }

  return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name))
}

// The `$query` being typed at the caret, including the rest of the token after it.
export function mentionAt(text: string, caret: number): Mention | null {
  const match = TYPING.exec(text.slice(0, caret))
  if (!match)
    return null
  const rest = /^[a-z0-9-]*/.exec(text.slice(caret))![0]
  if (/^[\w$]/.test(text.slice(caret + rest.length)))
    return null
  return { start: caret - match[0].length, end: caret + rest.length, query: match[1]! }
}

function rank(skill: SkillOption, query: string) {
  const name = skill.name
  if (!query)
    return 0
  if (name === query)
    return 0
  if (name.startsWith(query))
    return 1
  if (name.split('-').some(part => part.startsWith(query)))
    return 2
  if (name.includes(query))
    return 3
  let index = 0
  for (const char of name) {
    if (char === query[index])
      index++
  }

  if (index === query.length)
    return 4
  return skill.description.toLowerCase().includes(query) ? 5 : -1
}

export function matchSkills(skills: SkillOption[], query: string, limit = 50) {
  return skills
    .map(skill => ({ skill, score: rank(skill, query) }))
    .filter(item => item.score >= 0)
    .sort((a, b) => a.score - b.score || (query ? a.skill.name.length - b.skill.name.length : 0) || a.skill.name.localeCompare(b.skill.name))
    .slice(0, limit)
    .map(item => item.skill)
}

export function insertSkill(text: string, mention: Mention, name: string) {
  const after = text.slice(mention.end)
  const token = `$${name}${/^\s/.test(after) ? '' : ' '}`
  return { text: text.slice(0, mention.start) + token + after, caret: mention.start + token.length + (/^\s/.test(after) ? 1 : 0) }
}

// Split text so recognised `$skill` tokens can be highlighted.
export function mentionSegments(text: string, names: ReadonlySet<string>): MentionSegment[] {
  const code: [number, number][] = [...text.matchAll(CODE)].map(match => [match.index, match.index + match[0].length])
  const segments: MentionSegment[] = []
  let last = 0
  for (const match of text.matchAll(MENTION)) {
    const name = match[1]!
    if (!names.has(name) || code.some(([start, end]) => match.index >= start && match.index < end))
      continue
    if (match.index > last)
      segments.push({ text: text.slice(last, match.index) })
    segments.push({ text: match[0], skill: name })
    last = match.index + match[0].length
  }

  if (last < text.length || !segments.length)
    segments.push({ text: text.slice(last) })
  return segments
}
