import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'

async function main() {
  process.chdir(path.dirname(process.argv[1]))
  const mode = process.argv[2]
  const ack = process.argv[3]
  const api = process.argv[4] || '34'
  assert.match(api, /^(?:29|[3-9]\d)$/)
  const system = process.argv[5] || 'google-apis'
  assert.ok(['aosp', 'google-apis'].includes(system))
  const env = JSON.parse(execFileSync('/usr/local/bin/leo', ['toolkit-env'], { encoding: 'utf8' }))
  const run = (bin, args) => {
    process.stdout.write(`probe.command ${path.basename(bin)} ${bin === 'adb' ? args.slice(2, 5).join(' ') : ''}\n`)
    const timeout = bin !== 'adb' ? 1200000 : args.includes('uiautomator') ? 30000 : 120000
    return execFileSync(bin, args, {
      env,
      encoding: 'utf8',
      timeout,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
  }

  run('cc', ['-Wall', '-Wextra', '-Werror', '-O2', 'nested-kvm.c', '-o', 'nested-kvm'])
  const nested = run('./nested-kvm', [])
  assert.match(nested, /KVM_RUN, rax=42, HLT/)
  process.stdout.write(nested)
  assert.equal(env.ANDROID_HOME, '/home/node/.local/share/android/sdk')
  const boot = fs.readFileSync('/proc/sys/kernel/random/boot_id', 'utf8')
  const saved = path.join(env.HOME, 'android-probe.json')
  if (mode === 'resume') {
    const previous = JSON.parse(fs.readFileSync(saved, 'utf8'))
    assert.notEqual(previous.boot, boot)
    assert.equal(fs.statSync(path.join(env.ANDROID_HOME, 'cmdline-tools/latest/bin/sdkmanager')).mtimeMs, previous.sdkMtime)
  }

  const start = Date.now()
  process.stdout.write(run('leo-android', ['setup', '--accept-licenses', 'platforms;android-34', 'build-tools;34.0.0']))
  process.stdout.write(run('leo-android', ['emulator', 'start', api, ...(system === 'aosp' ? ['--aosp'] : []), '--accept-licenses']))
  const device = JSON.parse(run('leo-android', ['emulator', 'status']))
  assert.equal(device.state, 'ready')
  assert.equal(device.image || 'google-apis', system)
  assert.equal(device.acceleration, 'kvm', 'Android must use nested KVM, not software emulation')
  process.stdout.write(`${JSON.stringify({
    bootAndSetupMs: Date.now() - start,
    mode,
    api,
    system,
  })}\n`)
  const adb = args => run('adb', ['-s', 'emulator-5580', ...args])
  assert.equal(adb(['shell', 'getprop', 'sys.boot_completed']).trim(), '1')
  for (const setting of ['window_animation_scale', 'transition_animation_scale', 'animator_duration_scale'])
    adb(['shell', 'settings', 'put', 'global', setting, '0'])
  adb(['shell', 'input', 'keyevent', 'KEYCODE_WAKEUP'])
  adb(['shell', 'wm', 'dismiss-keyguard'])
  if (mode === 'first') {
    fs.mkdirSync('classes', { recursive: true })
    fs.writeFileSync('MainActivity.java', 'package com.leo.fixture; public class MainActivity extends android.app.Activity { public void onCreate(android.os.Bundle b) { super.onCreate(b); android.widget.Button v = new android.widget.Button(this); v.setText("Tap to verify"); v.setContentDescription("Verify device"); v.setOnClickListener(w -> {v.setText("Device test passed"); getPreferences(0).edit().putBoolean("passed",true).commit();}); if(getPreferences(0).getBoolean("passed",false))v.setText("Device test passed"); setContentView(v); } }')
    fs.writeFileSync('AndroidManifest.xml', '<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.leo.fixture"><application android:theme="@android:style/Theme.Material.Light.NoActionBar" android:label="Leo device probe"><activity android:name=".MainActivity" android:exported="true"/></application></manifest>')
    const tools = path.join(env.ANDROID_HOME, 'build-tools/34.0.0')
    const android = path.join(env.ANDROID_HOME, 'platforms/android-34/android.jar')
    run('javac', ['-source', '8', '-target', '8', '-classpath', android, '-d', 'classes', 'MainActivity.java'])
    run('mise', ['exec', '--', `${tools}/d8`, '--lib', android, '--output', '.', 'classes/com/leo/fixture/MainActivity.class'])
    run(`${tools}/aapt2`, ['link', '-I', android, '--manifest', 'AndroidManifest.xml', '--min-sdk-version', '23', '--target-sdk-version', '34', '-o', 'unsigned.apk'])
    run('zip', ['-j', 'unsigned.apk', 'classes.dex'])
    run(`${tools}/zipalign`, ['-f', '4', 'unsigned.apk', 'probe.apk'])
    if (!fs.existsSync('fixture.jks'))
      run('keytool', ['-genkeypair', '-keystore', 'fixture.jks', '-storepass', 'android', '-keypass', 'android', '-alias', 'fixture', '-keyalg', 'RSA', '-dname', 'CN=Fixture', '-validity', '1'])
    run('mise', ['exec', '--', `${tools}/apksigner`, 'sign', '--ks', 'fixture.jks', '--ks-pass', 'pass:android', 'probe.apk'])
    adb(['install', '--no-incremental', '-r', 'probe.apk'])
    adb(['shell', 'pm', 'clear', 'com.leo.fixture'])
  }

  adb(['shell', 'am', 'start', '-W', '-n', 'com.leo.fixture/.MainActivity'])

  async function dump() {
    const deadline = Date.now() + 120000
    while (true) {
      try {
        adb(['shell', 'uiautomator', 'dump', '/sdcard/probe.xml'])
        return adb(['shell', 'cat', '/sdcard/probe.xml'])
      }
      catch (error) {
        if (Date.now() >= deadline)
          throw error
        await sleep(2000)
      }
    }
  }

  function tap(node) {
    const [x1, y1, x2, y2] = node.match(/bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"/).slice(1).map(Number)
    adb(['shell', 'input', 'tap', `${Math.floor((x1 + x2) / 2)}`, `${Math.floor((y1 + y2) / 2)}`])
  }

  let systemWaits = 0

  async function screen() {
    while (true) {
      const xml = await dump()
      // Cold boots may briefly stall Android's own UI or system process.
      // Allow two explicit waits in total; application ANRs remain failures.
      const wait = (xml.includes('System UI isn\'t responding') || xml.includes('Process system isn\'t responding')) && xml.match(/<node[^>]*resource-id="android:id\/aerr_wait"[^>]*>/)?.[0]
      if (!wait || systemWaits >= 2)
        return xml
      systemWaits++
      process.stdout.write('probe.system-ui.wait\n')
      tap(wait)
      await sleep(2000)
    }
  }

  async function waitForScreen(matches) {
    const deadline = Date.now() + 120000
    let xml
    do {
      xml = await screen()
      if (matches(xml))
        return xml
      await sleep(1000)
    } while (Date.now() < deadline)

    assert.fail(`Expected application view did not appear: ${xml}`)
  }

  if (mode === 'first') {
    const xml = await waitForScreen(xml => xml.includes('content-desc="Verify device"'))
    const node = xml.match(/<node[^>]*content-desc="Verify device"[^>]*>/)?.[0]
    assert.ok(node, xml)
    tap(node)
  }

  assert.match(await waitForScreen(xml => /Device test passed/i.test(xml)), /Device test passed/i)
  fs.writeFileSync('device.png', execFileSync('adb', ['-s', 'emulator-5580', 'exec-out', 'screencap', '-p'], { env, timeout: 30000 }))
  fs.writeFileSync(saved, JSON.stringify({ boot, sdkMtime: fs.statSync(path.join(env.ANDROID_HOME, 'cmdline-tools/latest/bin/sdkmanager')).mtimeMs }))
  process.stdout.write('probe.capture\n')
  for (let i = 0; !fs.existsSync(path.join(ack, 'ok')); i++) {
    assert.ok(i < 300)
    await sleep(100)
  }

  fs.rmSync(ack, { recursive: true })
  process.stdout.write(run('leo-android', ['emulator', 'stop']))
  process.stdout.write('probe.done\n')
}

main().catch((error) => {
  try {
    const env = JSON.parse(execFileSync('/usr/local/bin/leo', ['toolkit-env'], { encoding: 'utf8' }))
    // This disposable device contains only the synthetic probe application.
    const logcat = execFileSync('adb', ['-s', 'emulator-5580', 'logcat', '-b', 'crash', '-b', 'system', '-d', '-t', '200'], { env, encoding: 'utf8', timeout: 15000 })
    process.stderr.write(`Android failure diagnostics:\n${logcat}\n`)
  }
  catch {}

  const log = '/home/node/.android/leo-emulator.log'
  if (fs.existsSync(log))
    process.stderr.write(`Emulator log:\n${fs.readFileSync(log, 'utf8').slice(-20000).replace(/^.*(?:adb public key|adb.pubkey).*$/gm, '<REDACTED>')}\n`)
  process.stderr.write(`${error.message}\n${error.stdout?.toString() || ''}\n${error.stderr?.toString() || ''}`)
  process.exitCode = 1
})
