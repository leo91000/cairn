import { api } from './api'

/** Installation continues on the manager while each relay request stays short. */
export async function pluginOperation(path: string, body: unknown): Promise<unknown> {
  const result = await api<{ operation?: string }>(path, { method: 'POST', body: JSON.stringify(body) })
  if (!result.operation)
    return result

  const deadline = Date.now() + 10 * 60 * 1000
  while (Date.now() < deadline) {
    const operation = await api<{ state: string, result?: unknown, error?: string }>(`/plugins/packages/operations/${result.operation}`)
    if (operation.state === 'completed')
      return operation.result
    if (operation.state === 'failed')
      throw new Error(operation.error || 'Plugin preparation failed.')
    await new Promise(resolve => setTimeout(resolve, 500))
  }
  throw new Error('Preparation is still running. Refresh the plugin to check its state.')
}
