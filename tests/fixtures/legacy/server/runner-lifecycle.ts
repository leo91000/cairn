import { access, mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'

// Attempts use unique IDs. A durable stop marker rejects delayed POSTs even after
// the broker restarts; serialization fences an already in-flight container start.
export class RunnerLifecycle {
  private operations = new Map<string, Promise<void>>()
  constructor(readonly directory: string, readonly create: (id: string) => Promise<void>, readonly remove: (id: string) => Promise<void>) {}

  private serial(id: string, operation: () => Promise<void>) {
    const next = (this.operations.get(id) ?? Promise.resolve()).catch(() => {}).then(operation)
    this.operations.set(id, next)
    void next.finally(() => {
      if (this.operations.get(id) === next)
        this.operations.delete(id)
    }).catch(() => {})
    return next
  }

  start(id: string) {
    return this.serial(id, async () => {
      if (await access(path.join(this.directory, `${id}.stopped`)).then(() => true).catch((error) => {
        if (error.code !== 'ENOENT')
          throw error
        return false
      })) {
        throw new Error('This execution attempt has already stopped.')
      }

      await this.create(id)
    })
  }

  async stop(id: string) {
    await mkdir(this.directory, { recursive: true, mode: 0o700 })
    await writeFile(path.join(this.directory, `${id}.stopped`), '', { mode: 0o600 })
    await this.serial(id, () => this.remove(id))
  }
}
