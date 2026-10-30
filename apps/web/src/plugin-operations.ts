import type { PluginOperationResult } from '../../../packages/contracts/generated/typescript/PluginOperationResult'
import { getPluginOperation } from '../../../packages/contracts/generated/getPluginOperation'
import { api, ApiError } from './api'

/** Installation continues on the manager while each relay request stays short. */
export async function pluginOperation<T = unknown>(path: string, body?: unknown, method = 'POST'): Promise<T> {
  return pluginRequest(path, { method, body: body === undefined ? undefined : JSON.stringify(body) })
}

export async function pluginRequest<T>(path: string, options: RequestInit): Promise<T> {
  const result = await api<T & { operation?: string }>(path, options)
  if (!result.operation)
    return result

  const deadline = Date.now() + 10 * 60 * 1000
  while (Date.now() < deadline) {
    const operation = await getPluginOperation(api, { operationId: result.operation })
    if (operation.state === 'completed')
      return operationResult(operation.result!) as T
    if (operation.state === 'failed' || operation.state === 'cancelled')
      throw new ApiError(operation.error || 'Plugin preparation failed.', operation.errorStatus ?? 502)
    await new Promise(resolve => setTimeout(resolve, 500))
  }

  throw new Error('Preparation is still running. Refresh the plugin to check its state.')
}

function operationResult(result: PluginOperationResult): unknown {
  switch (result.kind) {
    case 'project': return result.project
    case 'sourceProjects': return result.page
    case 'skillImport': return result.imported
    case 'skillSource': return result.status
    case 'skillBundle': return result.bundle
    case 'package': return result.version
    case 'program': return result.program
    case 'management': {
      if (result.wrapped)
        return result.output
      const output = result.output
      switch (output.kind) {
        case 'form': return output.form
        case 'saved': case 'enabled': case 'people': return output.instance
        case 'view': return { view: output.view }
        case 'action': return output.result
        case 'signin': return output.flow
        case 'signinCancelled': return { cancelled: output.cancelled }
        case 'setup': return output
        default: return output
      }
    }
  }
}
