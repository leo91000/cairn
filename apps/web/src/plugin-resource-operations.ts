// Resource listings and ancestry resolutions share the manager's four preparation
// slots across every picker. Keep a slot until pluginRequest finishes polling.
const MAX_RESOURCE_PREPARATIONS = 4
let active = 0
const waiting: Array<() => void> = []

export async function runPluginResourcePreparation<T>(prepare: () => Promise<T>): Promise<T> {
  if (active < MAX_RESOURCE_PREPARATIONS)
    active++
  else
    await new Promise<void>(resolve => waiting.push(resolve))

  try {
    for (let attempt = 0; ; attempt++) {
      try {
        return await prepare()
      }
      catch (error) {
        // Other people or non-resource preparations can temporarily occupy slots.
        // Retry only capacity failures, never authorization or plugin failures.
        const status = error && typeof error === 'object' && 'status' in error ? error.status : undefined
        if (status !== 429 || attempt === 4)
          throw error
        await new Promise(resolve => setTimeout(resolve, 250 * 2 ** attempt))
      }
    }
  }
  finally {
    const next = waiting.shift()
    if (next)
      next()
    else
      active--
  }
}
