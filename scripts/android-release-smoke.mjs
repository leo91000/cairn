import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'

const exec = promisify(execFile)
export const smokeProbe = 'webrtc-initialize-factory-dispose-v1'

export function verifySmokeOutput(instrumentation, logcat) {
  if (/JNI DETECTED ERROR|Fatal signal|FATAL EXCEPTION/i.test(logcat))
    throw new Error('Release smoke detected a Java/JNI/native crash; see logcat.txt')
  if (!instrumentation.split(/\r?\n/).includes(`INSTRUMENTATION_RESULT: cairnReleaseSmoke=${smokeProbe}`)
    || !instrumentation.split(/\r?\n/).includes('INSTRUMENTATION_CODE: -1')) {
    throw new Error('Release WebRTC initialization did not complete; see instrumentation.txt')
  }
}

export function hasJniZeroInit(dump) {
  return dump.split(/Class #\d+/).some(section =>
    /Class descriptor\s*:\s*'Lorg\/jni_zero\/JniZero;'/.test(section)
    && /name\s*:\s*'init'\s+type\s*:\s*'\(\)\[Ljava\/lang\/Object;'/.test(section))
}

async function sha256(file) {
  return createHash('sha256').update(await readFile(file)).digest('hex')
}

export async function guardDex(apk, buildTools) {
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'cairn-release-dex-'))
  try {
    const listing = (await exec('unzip', ['-Z1', apk])).stdout
    for (const entry of listing.trim().split('\n').filter(name => /^classes\d*\.dex$/.test(name))) {
      const dex = path.join(temporary, entry)
      const bytes = (await exec('unzip', ['-p', apk, entry], { encoding: 'buffer', maxBuffer: 128 * 1024 * 1024 })).stdout
      await writeFile(dex, bytes)
      const dump = (await exec(path.join(buildTools, 'dexdump'), [dex], { maxBuffer: 128 * 1024 * 1024 })).stdout
      if (hasJniZeroInit(dump))
        return
    }

    throw new Error('Release DEX is missing org.jni_zero.JniZero.init ()[Ljava/lang/Object;')
  }
  finally {
    await rm(temporary, { recursive: true, force: true })
  }
}

export async function runSmoke(apk, testApk, output, buildTools) {
  await mkdir(output, { recursive: true })
  const evidenceFile = path.join(output, 'release-smoke.json')
  await rm(evidenceFile, { force: true })
  const temporary = await mkdtemp(path.join(os.tmpdir(), 'cairn-release-smoke-'))
  const adb = process.env.ANDROID_SERIAL ? ['-s', process.env.ANDROID_SERIAL] : []
  const device = args => exec('adb', [...adb, ...args], { timeout: 120000, maxBuffer: 16 * 1024 * 1024 })
  const digest = await sha256(apk)
  try {
    // Sign copies only. Publication keeps using the original unsigned APK, without a rebuild.
    const key = path.join(temporary, 'smoke.p12')
    await exec('keytool', ['-genkeypair', '-keystore', key, '-storepass', 'android', '-keypass', 'android', '-alias', 'smoke', '-dname', 'CN=Cairn release smoke', '-keyalg', 'RSA', '-validity', '2'])
    for (const [input, name] of [[apk, 'release.apk'], [testApk, 'probe.apk']]) {
      const signed = path.join(temporary, name)
      await exec(path.join(buildTools, 'apksigner'), ['sign', '--ks', key, '--ks-pass', 'pass:android', '--out', signed, input])
      await exec(path.join(buildTools, 'apksigner'), ['verify', signed])
      // A fresh isolated emulator is expected; clear previous runs and certificates.
      await device(['uninstall', name === 'release.apk' ? 'build.cairn.app' : 'build.cairn.smoke']).catch(() => {})
      await device(['install', signed])
    }

    await device(['logcat', '-c'])
    let instrumentation
    try {
      instrumentation = (await device(['shell', 'am', 'instrument', '-w', 'build.cairn.smoke/build.cairn.smoke.WebRtcSmoke'])).stdout
    }
    catch (error) {
      instrumentation = `${error.stdout || ''}\n${error.stderr || ''}\n${error.message}`
      throw error
    }
    finally {
      await writeFile(path.join(output, 'instrumentation.txt'), instrumentation || '')
      await writeFile(path.join(output, 'logcat.txt'), (await device(['logcat', '-d', '-v', 'threadtime'])).stdout)
    }

    verifySmokeOutput(instrumentation, await readFile(path.join(output, 'logcat.txt'), 'utf8'))
    if (await sha256(apk) !== digest)
      throw new Error('Unsigned release changed during smoke validation')
    const api = (await device(['shell', 'getprop', 'ro.build.version.sdk'])).stdout.trim()
    const abi = (await device(['shell', 'getprop', 'ro.product.cpu.abi'])).stdout.trim()
    await writeFile(evidenceFile, `${JSON.stringify({
      probe: smokeProbe,
      sha256: digest,
      api,
      abi,
    }, null, 2)}\n`)
    console.log(`Release WebRTC smoke passed: API ${api}, ${abi}, unsigned SHA-256 ${digest}`)
  }
  finally {
    await device(['uninstall', 'build.cairn.smoke']).catch(() => {})
    await device(['uninstall', 'build.cairn.app']).catch(() => {})
    await rm(temporary, { recursive: true, force: true })
  }
}

if (import.meta.main) {
  const [, , command, apk, testApk, output] = process.argv
  const buildTools = path.join(process.env.ANDROID_HOME, 'build-tools', '36.0.0')
  if (command === 'guard')
    await guardDex(apk, buildTools)
  else if (command === 'run')
    await runSmoke(apk, testApk, output, buildTools)
  else
    throw new Error('Expected guard APK or run APK PROBE_APK OUTPUT_DIRECTORY')
}
