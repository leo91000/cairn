import { createRequire } from 'node:module'
import { describe, expect, it } from 'vitest'
import * as icons from '../src/icons'

const require = createRequire(import.meta.url)

describe('compiled UI icons', () => {
  it('resolves every icon name to an installed Iconify collection', () => {
    const collections = new Map<string, { icons: Record<string, unknown>, aliases?: Record<string, unknown> }>()
    for (const name of ['lucide', 'tabler', 'simple-icons'])
      collections.set(name, require(`@iconify-json/${name}/icons.json`))

    for (const [name, className] of Object.entries(icons)) {
      const collectionName = [...collections.keys()].find(prefix => className.startsWith(`i-${prefix}-`))!
      const collection = collections.get(collectionName)
      expect(collection, name).toBeDefined()
      const icon = className.slice(`i-${collectionName}-`.length)
      expect(Object.hasOwn(collection!.icons, icon) || Object.hasOwn(collection!.aliases ?? {}, icon), name).toBe(true)
    }
  })
})
