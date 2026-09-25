import { createHash } from 'node:crypto'
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

export function androidVersion(tag) {
  if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(tag))
    throw new Error('Android releases require a stable vMAJOR.MINOR.PATCH tag')
  const versionName = tag.slice(1)
  const [major, minor, patch] = versionName.split('.').map(Number)
  if (major > 1999 || minor > 999 || patch > 999)
    throw new Error('Android version components exceed versionCode bounds')
  return { versionName, versionCode: 100_000_000 + major * 1_000_000 + minor * 1000 + patch }
}

export function updateManifest(tag, bytes, metadata) {
  const version = androidVersion(tag)
  if (metadata.applicationId !== 'dev.leo.manager' || metadata.elements?.length !== 1
    || metadata.elements[0].versionCode !== version.versionCode
    || metadata.elements[0].versionName !== version.versionName) {
    throw new Error('Built APK metadata does not match the release tag')
  }

  if (!bytes.length || bytes.length > 100 * 1024 * 1024)
    throw new Error('Invalid APK size')
  return {
    schema: 1,
    ...version,
    minSdk: 26,
    url: `https://github.com/leo91000/leo-agent-manager/releases/download/${tag}/leo-android.apk`,
    sha256: createHash('sha256').update(bytes).digest('hex'),
    size: bytes.length,
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [, , tag, apk, metadata, destination] = process.argv
  const manifest = updateManifest(tag, readFileSync(apk), JSON.parse(readFileSync(metadata, 'utf8')))
  writeFileSync(destination, `${JSON.stringify(manifest, null, 2)}\n`)
}
