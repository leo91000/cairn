import { api } from './api'

/** Installation continues on the manager while each relay request stays short. */
export async function pluginOperation<T = unknown>(path: string, body?: unknown, method = 'POST'): Promise<T> {
  const result = await api<T & { operation?: string }>(path, { method, body: body === undefined ? undefined : JSON.stringify(body) })
  if (!result?.operation)
    return result

  const deadline = Date.now() + 10 * 60 * 1000
  while (Date.now() < deadline) {
    const operation = await api<{ state: string, result?: unknown, error?: string }>(`/plugins/packages/operations/${result.operation}`)
    if (operation.state === 'completed')
      return operation.result as T
    if (operation.state === 'failed')
      throw new Error(operation.error || 'Plugin preparation failed.')
    await new Promise(resolve => setTimeout(resolve, 500))
  }

  throw new Error('Preparation is still running. Refresh the plugin to check its state.')
}
