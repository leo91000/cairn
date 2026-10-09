import type { Store } from './store.ts'
import { Buffer } from 'node:buffer'
import { createCipheriv, createDecipheriv, randomBytes } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'

export class McpVault {
  private key: Buffer
  constructor(private store: Store, directory: string) {
    const filename = path.join(directory, 'mcp-encryption-key')
    try {
      writeFileSync(filename, randomBytes(32), { flag: 'wx', mode: 0o600 })
    }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EEXIST')
        throw error
    }

    this.key = readFileSync(filename)
    if (this.key.length !== 32)
      throw new Error('Invalid MCP encryption key.')
  }

  get<T>(id: string): T | undefined {
    const data = this.store.kv<string>(`mcp-secret:${id}`)
    if (!data)
      return
    const bytes = Buffer.from(data, 'base64')
    const cipher = createDecipheriv('aes-256-gcm', this.key, bytes.subarray(0, 12))
    cipher.setAAD(Buffer.from(id))
    cipher.setAuthTag(bytes.subarray(12, 28))
    return JSON.parse(Buffer.concat([cipher.update(bytes.subarray(28)), cipher.final()]).toString())
  }

  set(id: string, value: unknown) {
    const iv = randomBytes(12)
    const cipher = createCipheriv('aes-256-gcm', this.key, iv)
    cipher.setAAD(Buffer.from(id))
    const encrypted = Buffer.concat([cipher.update(JSON.stringify(value)), cipher.final()])
    this.store.set(`mcp-secret:${id}`, Buffer.concat([iv, cipher.getAuthTag(), encrypted]).toString('base64'))
  }

  delete(id: string) { this.store.delete(`mcp-secret:${id}`) }
}
