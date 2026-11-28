import type { HookRule } from '../../packages/contracts/generated/typescript/HookRule'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import { expect } from './fixtures'

export function scheduleDefinition(name: string, agentId: string, prompt: string, projectId?: string) {
  return {
    name,
    description: '',
    executionAgentId: agentId,
    trigger: { type: 'clock', cron: '0 9 * * *', timezone: 'Europe/Paris' },
    limits: { triggeringsPerHour: 20, conversationsPerHour: 10 },
    steps: [{
      id: 'avoid_overlap',
      type: 'if',
      condition: { type: 'not', condition: { type: 'ruleConversationWorking' } },
      then: [{
        id: 'start_agent',
        type: 'action',
        action: {
          type: 'startAgent',
          agentId,
          projectId,
          prompt: { type: 'literal', value: prompt },
        },
      }],
      otherwise: [],
    }],
  }
}

export async function runSchedule(workspace: Workspace, rule: HookRule) {
  const before = new Set(workspace.service.store.list('chats').map(chat => chat.id))
  await workspace.api(`/api/hook-rules/${rule.id}/run`, 'POST')
  const created = async () => (await workspace.api('/api/chats')).find((chat: any) => chat.originRule?.ruleId === rule.id && !before.has(chat.id))
  await expect.poll(created, { timeout: 20000 }).toBeTruthy()
  return created()
}

export async function startConversation(workspace: Workspace, title: string, text: string, agentId?: string) {
  const chat = await workspace.api('/api/chats', 'POST', { agentId })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
  workspace.service.store.put('chats', { id: chat.id, title })
  let runId: string
  await expect.poll(() => {
    runId = workspace.service.chats.detail(chat.id).runId!
    return runId
  }, { timeout: 20000 }).toBeTruthy()
  return { chat, run: workspace.service.store.run(runId!) }
}
