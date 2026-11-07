import { randomUUID } from 'node:crypto'

// Use the real authenticated workspace MCP, never fixture-only server routes.
export async function askCairn(server, questions) {
  const response = await fetch(server.url, {
    method: 'POST',
    headers: {
      ...server.headers,
      ...server.http_headers,
      'Content-Type': 'application/json',
      'Accept': 'application/json, text/event-stream',
    },
    body: JSON.stringify({
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: { name: 'ask_questions', arguments: { requestId: randomUUID(), questions } },
    }),
  })
  if (!response.ok)
    throw new Error(`Question creation failed (${response.status})`)
  const result = await response.json()
  if (result.error || result.result?.isError)
    throw new Error(result.error?.message || result.result.content?.find(item => item.type === 'text')?.text || 'Question creation was refused')
  return result.result.structuredContent.questionId
}

export const navigationQuestions = [{
  id: 'direction',
  title: 'How should we introduce the new navigation?',
  mode: 'multiple',
  minChoices: 2,
  maxChoices: 2,
  options: [
    { id: 'chat', label: 'Chat view', recommended: true },
    { id: 'android', label: 'Android', recommended: true },
    { id: 'web', label: 'Web' },
  ],
}]
