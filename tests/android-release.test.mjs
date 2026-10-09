import { Buffer } from 'node:buffer'
import { describe, expect, it } from 'vitest'
import { androidVersion, updateManifest } from '../scripts/android-release.mjs'

describe('android tag distribution', () => {
  it('assigns increasing codes above the old debug builds', () => {
    expect(androidVersion('v0.29.0')).toEqual({ versionName: '0.29.0', versionCode: 100029000 })
    expect(androidVersion('v1.0.0').versionCode).toBeGreaterThan(androidVersion('v0.999.999').versionCode)
    expect(androidVersion('v1999.999.999').versionCode).toBeLessThan(2100000000)
  })
  it.each(['v1.0.0-beta', 'v01.0.0', '1.0.0', 'v2000.0.0', 'v0.1000.0', 'v0.1.1000', '../bad'])('rejects %s', (tag) => {
    expect(() => androidVersion(tag)).toThrow()
  })
  it('publishes a checksum and refuses a stale or different APK', () => {
    const metadata = { applicationId: 'build.cairn.app', elements: [androidVersion('v0.29.0')] }
    const result = updateManifest('v0.29.0', Buffer.from('abc'), metadata)
    expect(result.sha256).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad')
    expect(result.size).toBe(3)
    expect(() => updateManifest('v0.29.1', Buffer.from('abc'), metadata)).toThrow()
    expect(() => updateManifest('v0.29.0', Buffer.from('abc'), { ...metadata, applicationId: 'another.app' })).toThrow()
  })
})
