import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { updateManifest } from './android-release.mjs'

export const artifactName = 'validated-android-release'

const firebaseKeys = ['APP_ID', 'PROJECT_ID', 'API_KEY', 'SENDER_ID']

export function firebaseConfigurationFromEnvironment(environment = process.env) {
  return Object.fromEntries(firebaseKeys.map(key => [key, environment[`LEO_ANDROID_FIREBASE_${key}`] || '']))
}

function firebaseBuildConfiguration(configuration = {}) {
  return Object.fromEntries(firebaseKeys.map(key => [key, configuration[key] || '']))
}

function fixedOrigin(value) {
  if (!value)
    return ''
  const url = new URL(value)
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password
    || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('The official service must be an HTTP(S) origin without credentials or a path')
  }

  return url.origin
}

async function buildManifest(directory, tag) {
  const metadata = JSON.parse(await readFile(path.join(directory, 'output-metadata.json'), 'utf8'))
  if (metadata.elements?.[0]?.outputFile !== 'app-release-unsigned.apk')
    throw new Error('Expected the unsigned release APK')
  const apk = await readFile(path.join(directory, 'app-release-unsigned.apk'))
  return updateManifest(tag || `v${metadata.elements[0].versionName}`, apk, metadata)
}

export async function recordBuild(directory, config) {
  const manifest = await buildManifest(directory, config.tag)
  const evidence = {
    schema: 1,
    officialOrigin: fixedOrigin(config.officialOrigin),
    firebaseConfiguration: firebaseBuildConfiguration(config.firebaseConfiguration),
    repository: config.repository.toLowerCase(),
    commit: config.commit,
    runId: config.runId,
    versionName: manifest.versionName,
    versionCode: manifest.versionCode,
    sha256: manifest.sha256,
    size: manifest.size,
  }
  await writeFile(path.join(directory, 'validation.json'), `${JSON.stringify(evidence, null, 2)}\n`)
  return evidence
}

export async function verifyBuild(directory, config) {
  const officialOrigin = fixedOrigin(config.officialOrigin)
  if (!officialOrigin.startsWith('https://'))
    throw new Error('Android distribution requires the fixed HTTPS official service origin')
  const evidence = JSON.parse(await readFile(path.join(directory, 'validation.json'), 'utf8'))
  const manifest = await buildManifest(directory, config.tag)
  if (JSON.stringify(evidence.firebaseConfiguration) !== JSON.stringify(firebaseBuildConfiguration(config.firebaseConfiguration))
    || evidence.schema !== 1 || evidence.officialOrigin !== officialOrigin || evidence.repository !== config.repository.toLowerCase()
    || evidence.commit !== config.commit || evidence.runId !== config.runId
    || evidence.versionName !== manifest.versionName || evidence.versionCode !== manifest.versionCode
    || evidence.sha256 !== manifest.sha256 || evidence.size !== manifest.size) {
    throw new Error('Android build evidence does not match this release')
  }

  return evidence
}

if (import.meta.main) {
  const [, , command, directory] = process.argv
  const config = {
    repository: process.env.GITHUB_REPOSITORY,
    commit: process.env.GITHUB_SHA,
    runId: Number(process.env.VALIDATED_RUN_ID || process.env.GITHUB_RUN_ID),
    tag: process.env.RELEASE_TAG || undefined,
    officialOrigin: process.env.LEO_OFFICIAL_ORIGIN,
    firebaseConfiguration: firebaseConfigurationFromEnvironment(),
  }
  if (command === 'record')
    await recordBuild(directory, config)
  else if (command === 'verify')
    await verifyBuild(directory, config)
  else
    throw new Error('Expected record or verify and an APK directory')
}
