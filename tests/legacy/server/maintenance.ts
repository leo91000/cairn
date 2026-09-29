import type { Store } from './store.ts'
import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { safeEqual, token } from './auth.ts'
import { AppError } from './errors.ts'

export function maintenanceActive(store: Store) {
  return !!store.kv('deployment-lease')
}

export async function maintenanceToken(directory: string) {
  const filename = path.join(directory, 'maintenance-token')
  try {
    await writeFile(filename, token(), { flag: 'wx', mode: 0o600 })
  }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST')
      throw error
  }

  return (await readFile(filename, 'utf8')).trim()
}

export function deploymentLease(store: Store, secret: string, authorization: string | undefined, owner: string, release = false) {
  if (!safeEqual(authorization ?? '', `Bearer ${secret}`))
    throw new AppError(401, 'Invalid maintenance credential.')
  const existing = store.kv<string>('deployment-lease')
  if (existing && existing !== owner)
    throw new AppError(409, 'Another deployment holds the worker lease.')
  if (release)
    store.delete('deployment-lease')
  else
    store.set('deployment-lease', owner, Date.now() + 20 * 60000)
  return { paused: !release }
}
