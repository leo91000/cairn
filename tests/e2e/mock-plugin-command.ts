import type { Page, Route } from '@playwright/test'

/**
 * Scenario callbacks model the package's form/panel and its instance effect. The
 * browser still sends the real generated command; this fixture projects its
 * manifest input and returns the typed Cairn envelope around the package reply.
 */
export async function mockPluginCommand(page: Page, pattern: string, handler: (route: Route) => unknown) {
  const matcher = new RegExp(`^${pattern.split('*').map(part => part.replace(/[.+?^${}()|[\]\\]/g, '\\$&')).join('.*')}$`)
  await page.route('**/plugins/manage', async (route) => {
    const command = route.request().postDataJSON()
    const base = `/plugins/${command.pluginId}`
    const instance = `${base}/connections${command.instanceId ? `/${command.instanceId}` : ''}`
    const paths: Record<string, string> = {
      form: `${base}/form`,
      save: instance,
      enabled: `${instance}/enabled`,
      delete: instance,
      disconnect: `${instance}/disconnect`,
      test: `${instance}/test`,
      view: `${instance}/view`,
      action: `${instance}/actions`,
      people: `${instance}/people/${command.userId}/${command.operation}`,
    }
    const logicalUrl = route.request().url().replace(/\/plugins\/manage$/, paths[command.kind] ?? '')
    if (!matcher.test(logicalUrl))
      return route.fallback()
    const data = command.kind === 'form' ? { ...command.data, ...(command.instanceId ? { instance: command.instanceId } : {}) } : command.data
    const request = route.request()
    const proxyRequest = new Proxy(request, {
      get(target, property) {
        if (property === 'postDataJSON')
          return () => data
        if (property === 'url')
          return () => logicalUrl
        const value = Reflect.get(target, property)
        return typeof value === 'function' ? value.bind(target) : value
      },
    })
    const proxy = new Proxy(route, {
      get(target, property) {
        if (property === 'request')
          return () => proxyRequest
        if (property === 'continue') {
          return (options?: { postData?: string }) => target.continue(options?.postData
            ? { ...options, postData: JSON.stringify({ ...command, data: JSON.parse(options.postData) }) }
            : options)
        }

        if (property === 'fulfill') {
          return (options: Parameters<Route['fulfill']>[0] = {}) => {
            if (options.status && options.status >= 400)
              return target.fulfill(options)
            const value = options.json ?? JSON.parse(String(options.body ?? '{}'))
            const responses: Record<string, unknown> = {
              form: { kind: 'form', form: value },
              save: { kind: 'saved', instance: value },
              enabled: { kind: 'enabled', instance: value },
              delete: { kind: 'deleted', ...value },
              disconnect: { kind: 'disconnected', ...value },
              test: { kind: 'tested', ...value },
              view: { kind: 'view', ...value },
              action: { kind: 'action', result: value },
              people: { kind: 'people', instance: value },
            }
            return target.fulfill({ ...options, body: undefined, json: responses[command.kind] })
          }
        }

        const value = Reflect.get(target, property)
        return typeof value === 'function' ? value.bind(target) : value
      },
    })
    await handler(proxy)
  })
}
