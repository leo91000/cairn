import { expect, it } from 'vitest'
import { MAIN_AGENT_ID } from '../../../packages/contracts/constants'
import {
  accessRows,
  accessSummary,
  canDirectChat,
  canUseAgent,
  defaultAgent,
} from '../src/agent-use'

const reviewer = '00000000-0000-4000-8000-000000000002'
const writer = '00000000-0000-4000-8000-000000000003'

it('lets the owner use every agent and members only theirs', () => {
  expect(canUseAgent(null, reviewer)).toBe(true)
  expect(canUseAgent([reviewer], reviewer)).toBe(true)
  expect(canUseAgent([reviewer], writer)).toBe(false)
  expect(canUseAgent([], undefined)).toBe(false)
})

it('directs a delegated task through its requesting conversation while it exists', () => {
  const requesting = { id: 'requesting', agentId: reviewer }
  const delegated = { agentId: writer, requestedBy: { runId: 'run', chatId: 'requesting' } }
  expect(canDirectChat([reviewer], delegated, [requesting])).toBe(true)
  expect(canDirectChat([reviewer], delegated, [])).toBe(false)
  expect(canDirectChat([writer], delegated, [])).toBe(true)
  expect(canDirectChat([], { agentId: writer, requestedBy: null }, [requesting])).toBe(false)
})

it('starts new conversations on Main only when it is usable', () => {
  const agents = [{ id: MAIN_AGENT_ID }, { id: reviewer }, { id: writer }]
  expect(defaultAgent(null, agents)).toBe(MAIN_AGENT_ID)
  expect(defaultAgent([writer], agents)).toBe(writer)
  expect(defaultAgent([], agents)).toBe('')
})

it('lists every member once and keeps entries of former members removable', () => {
  const rows = accessRows([{ id: 'a', email: 'ada@example.com' }, { id: 'b', email: 'bo@example.com' }], ['b', 'gone'])
  expect(rows).toEqual([
    {
      accountId: 'a',
      email: 'ada@example.com',
      known: true,
      allowed: false,
    },
    {
      accountId: 'b',
      email: 'bo@example.com',
      known: true,
      allowed: true,
    },
    {
      accountId: 'gone',
      email: 'Former member',
      known: false,
      allowed: true,
    },
  ])
  expect(accessSummary(0)).toEqual({ short: 'Only you', long: 'Only you' })
  expect(accessSummary(1)).toEqual({ short: 'You + 1', long: 'You and 1 member' })
  expect(accessSummary(2)).toEqual({ short: 'You + 2', long: 'You and 2 members' })
})
