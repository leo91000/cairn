import type { Agent, Skill } from '../shared/contracts'
import { describe, expect, it } from 'vitest'
import {
  chatSkills,
  insertSkill,
  matchSkills,
  mentionAt,
  mentionSegments,
} from '../src/skill-mentions'

const project = '11111111-1111-4111-8111-111111111111'
const other = '22222222-2222-4222-8222-222222222222'

function skill(name: string, scope = 'global', extra: Partial<Skill> = {}): Skill {
  return {
    name,
    scope,
    description: `${name} description`,
    content: '',
    path: '',
    valid: true,
    ...extra,
  }
}

function agent(access: Partial<Agent['access']> = {}) {
  return ({
    access: {
      projects: null,
      skills: null,
      mcps: null,
      mcpTools: {},
      github: true,
      sandbox: 'yolo',
      ...access,
    },
  }) as unknown as Agent
}

describe('skill mentions', () => {
  it('offers valid skills the chat agent can use in its project', () => {
    const skills = [skill('review'), skill('deploy', project), skill('secret', other), skill('broken', 'global', { valid: false }), skill('review', project, { description: 'Project review' })]
    expect(chatSkills(skills, agent(), project).map(s => `${s.scope}/${s.name}`)).toEqual([`${project}/deploy`, `${project}/review`])
    expect(chatSkills(skills, agent(), null).map(s => s.name)).toEqual(['deploy', 'review', 'secret'])
    expect(chatSkills(skills, agent({ projects: [project], skills: ['global/review', `${other}/secret`] }), null).map(s => `${s.scope}/${s.name}`)).toEqual(['global/review'])
    expect(chatSkills(skills, undefined, null)).toEqual([])
  })

  it('finds the token being typed at the caret', () => {
    expect(mentionAt('Use $rev', 8)).toEqual({ start: 4, end: 8, query: 'rev' })
    expect(mentionAt('Use $review now', 6)).toEqual({ start: 4, end: 11, query: 'r' })
    expect(mentionAt('$', 1)).toEqual({ start: 0, end: 1, query: '' })
    expect(mentionAt('(\n$d', 4)).toEqual({ start: 2, end: 4, query: 'd' })
    expect(mentionAt('cost a$5', 8)).toBeNull()
    expect(mentionAt('echo $HOME', 7)).toBeNull()
    expect(mentionAt('\\$re', 4)).toBeNull()
    expect(mentionAt('Use $rev now', 12)).toBeNull()
  })

  it('ranks exact, prefix, word, substring, fuzzy and description matches', () => {
    const skills = ['code-review', 'review', 'reviewer', 'preview-docs', 'ship'].map(name => ({ name, description: name === 'ship' ? 'Release to production' : '', scope: 'global' }))
    expect(matchSkills(skills, 'review').map(s => s.name)).toEqual(['review', 'reviewer', 'code-review', 'preview-docs'])
    expect(matchSkills(skills, 'cr').map(s => s.name)).toEqual(['code-review'])
    expect(matchSkills(skills, 'production').map(s => s.name)).toEqual(['ship'])
    expect(matchSkills(skills, '').map(s => s.name)).toEqual(['code-review', 'preview-docs', 'review', 'reviewer', 'ship'])
    expect(matchSkills(skills, 'zzz')).toEqual([])
  })

  it('replaces the whole token and places the caret after one space', () => {
    expect(insertSkill('Use $rev', { start: 4, end: 8, query: 'rev' }, 'review')).toEqual({ text: 'Use $review ', caret: 12 })
    expect(insertSkill('Use $re now', { start: 4, end: 7, query: 're' }, 'review')).toEqual({ text: 'Use $review now', caret: 12 })
  })

  it('highlights known skills outside code only', () => {
    const names = new Set(['review'])
    expect(mentionSegments('Run $review, not $HOME or `$review` or $reviewer', names)).toEqual([
      { text: 'Run ' },
      { text: '$review', skill: 'review' },
      { text: ', not $HOME or `$review` or $reviewer' },
    ])
    expect(mentionSegments('', names)).toEqual([{ text: '' }])
  })
})
