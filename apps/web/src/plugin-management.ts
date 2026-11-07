import type { ManagePluginInput } from '../../../packages/contracts/generated/typescript/ManagePluginInput'
import type { ManagePluginOutput } from '../../../packages/contracts/generated/typescript/ManagePluginOutput'
import { managePlugin } from '../../../packages/contracts/generated/managePlugin'
import { pluginRequest } from './plugin-operations'

export async function pluginCommand<K extends ManagePluginOutput['kind']>(input: ManagePluginInput, kind: K): Promise<Extract<ManagePluginOutput, { kind: K }>> {
  const result = await managePlugin(pluginRequest, input)
  if (result.kind !== kind)
    throw new Error('Unexpected plugin response. Reload the instance and retry.')
  return result as Extract<ManagePluginOutput, { kind: K }>
}
