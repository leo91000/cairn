import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { updateManifest } from './android-release.mjs'

export const artifactName = 'validated-android-release'

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
  const evidence = JSON.parse(await readFile(path.join(directory, 'validation.json'), 'utf8'))
  const manifest = await buildManifest(directory, config.tag)
  if (evidence.schema !== 1 || evidence.repository !== config.repository.toLowerCase()
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
  }
  if (command === 'record')
    await recordBuild(directory, config)
  else if (command === 'verify')
    await verifyBuild(directory, config)
  else
    throw new Error('Expected record or verify and an APK directory')
}
