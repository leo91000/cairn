import type { FormLookup, FormResult } from './plugins'
import { pluginOperation } from './plugin-operations'
import { formResult } from './plugins'

export interface PluginFormLookup {
  context: (field: string) => unknown
  request: (lookup: FormLookup) => Promise<FormResult>
}

/** The editor retains credentials and scope. A field can only request choices. */
export function pluginFormLookup(path: () => string, body: () => Record<string, unknown>, generation: () => unknown = () => undefined): PluginFormLookup {
  return {
    context(field) {
      const { settings, ...scope } = body()
      const otherValues = { ...settings as Record<string, unknown> }
      delete otherValues[field]
      return {
        path: path(),
        generation: generation(),
        ...scope,
        settings: otherValues,
      }
    },
    async request(lookup) {
      const endpoint = path()
      const snapshot = JSON.stringify(body())
      const currentGeneration = JSON.stringify(generation())
      const result = formResult(await pluginOperation<FormResult>(endpoint, { ...JSON.parse(snapshot), lookup }))
      if (endpoint !== path() || snapshot !== JSON.stringify(body()) || currentGeneration !== JSON.stringify(generation()))
        throw new Error('The form changed. Search again.')
      // An older host may ignore lookup and return an ordinary form. Its answer
      // must never be mistaken for a validated search or applied as derived values.
      if (result.lookup?.field !== lookup.field || result.lookup?.query !== lookup.query || result.lookup?.cursor !== lookup.cursor)
        throw new Error('This server did not confirm the choice lookup. Update Cairn and try again.')
      return result
    },
  }
}

export function choicePageStatus(count: number, next: string | null | undefined) {
  const page = `${count} ${count === 1 ? 'choice' : 'choices'} on this page`
  return `${page} · ${next === undefined ? 'Completeness unknown' : next === null ? 'End of results' : 'More available'}`
}
