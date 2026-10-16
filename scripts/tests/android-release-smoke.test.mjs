import { describe, expect, it } from 'vitest'
import { hasJniZeroInit, smokeProbe, verifySmokeOutput } from '../android-release-smoke.mjs'

const success = `INSTRUMENTATION_RESULT: cairnReleaseSmoke=${smokeProbe}\nINSTRUMENTATION_CODE: -1\n`

describe('minified release WebRTC smoke verdict', () => {
  it('requires explicit completion of native initialization, factory creation and disposal', () => {
    expect(() => verifySmokeOutput(success, 'WebRTC initialized')).not.toThrow()
    for (const output of ['', 'INSTRUMENTATION_CODE: -1', success.replace('-1', '0'), success.replace(smokeProbe, 'other'), 'INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0'])
      expect(() => verifySmokeOutput(output, '')).toThrow('did not complete')
  })

  it.each(['JNI DETECTED ERROR IN APPLICATION: java_class == null', 'Fatal signal 6 (SIGABRT)', 'FATAL EXCEPTION: main'])('fails on %s even with a completion marker', (crash) => {
    expect(() => verifySmokeOutput(success, crash)).toThrow('crash')
  })
})

describe('cheap release DEX guard', () => {
  const dump = `Class #10
    Class descriptor  : 'Lorg/jni_zero/JniZero;'
    Direct methods    -
      name          : 'init'
      type          : '()[Ljava/lang/Object;'
Class #11
    Class descriptor  : 'Lother/Class;'
`
  it('requires the JNI class and the exact native entry point together', () => {
    expect(hasJniZeroInit(dump)).toBe(true)
    expect(hasJniZeroInit(dump.replace('JniZero;', 'Removed;'))).toBe(false)
    expect(hasJniZeroInit(dump.replace('()[Ljava/lang/Object;', '()V'))).toBe(false)
    expect(hasJniZeroInit(dump.replace('Direct methods', 'Class #12\nClass descriptor : \'Lother/Entry;\'\nDirect methods'))).toBe(false)
  })
})
