# Minified release WebRTC gate (#173)

The `release` job builds `:app:assembleRelease` with R8 and resource shrinking.
It first checks the DEX entry point `org.jni_zero.JniZero.init ()[Ljava/lang/Object;`,
then executes the release on an Android 16 x86_64 emulator. DEX presence alone
cannot pass the release gate.

`release-smoke` is an independent instrumentation APK with no WebRTC dependency
and no dependency on `:app`. Its runner loads WebRTC through the installed target
APK's class loader, initializes the native library, creates a native
`PeerConnectionFactory`, then disposes it. Reflection avoids introducing test
references or keep rules into the release shrinker inputs. No login, installation,
Beacon fixture, production secret or external network service is needed.

The host signs copies of both APKs with an ephemeral test key. It requires the
explicit completion marker and instrumentation success, and rejects Java/JNI/native
crashes in logcat. Timeouts, installation errors and process aborts fail the job.
The original unsigned release is hashed before and after the test and is never
modified or rebuilt for publication. The smoke proof is embedded in validation
schema 2, bound to that unsigned SHA-256 alongside the existing commit, workflow
run, version, Beacon and Firebase configuration checks. Legacy evidence without
this proof cannot be reused. Publication still verifies the unsigned artifact,
checks alignment, signs it with the distribution key, verifies the signature,
and produces the update manifest from the signed APK.

## Reproduce on an isolated emulator

Use the pinned JDK from `apps/android/mise.toml`, SDK platform 37.0 and build-tools
36.0.0. Start an isolated API 36 x86_64 emulator. The runner uninstalls
`build.cairn.app` and `build.cairn.smoke`; do not point it at a personal device.
From the repository root:

```sh
(cd apps/android && mise exec -- ./gradlew :app:assembleRelease :release-smoke:assembleDebug --no-daemon)
node scripts/android-release-smoke.mjs guard apps/android/app/build/outputs/apk/release/app-release-unsigned.apk
ANDROID_SERIAL=emulator-5580 node scripts/android-release-smoke.mjs run \
  apps/android/app/build/outputs/apk/release/app-release-unsigned.apk \
  apps/android/release-smoke/build/outputs/apk/debug/release-smoke-debug.apk \
  /tmp/cairn-release-smoke-fixed
```

The published faulty APK is also a stable negative control:

```sh
gh release download v0.53.0 --repo leo91000/cairn --pattern cairn-android.apk --dir /tmp/cairn-r8-negative
# SHA-256: 5702c4bde7738248726d58e09ad4deaa094654cfd2dcc9913189fe2e6bc85a02
ANDROID_SERIAL=emulator-5580 node scripts/android-release-smoke.mjs run \
  /tmp/cairn-r8-negative/cairn-android.apk \
  apps/android/release-smoke/build/outputs/apk/debug/release-smoke-debug.apk \
  /tmp/cairn-release-smoke-broken
```

To recreate the original R8 defect in a disposable checkout, remove only
`-keep class org.jni_zero.** { *; }` from `app/proguard-rules.pro`, rebuild
`:app:assembleRelease`, and run the **same `run` command** into another output
directory. Run it directly so the cheap DEX guard does not prevent the native
failure from being exercised. Expect a nonzero host exit, no `release-smoke.json`,
and `JNI DETECTED ERROR` / `Fatal signal` in logcat. Restore the keep rule, rebuild,
and require a successful run with a proof matching the unsigned APK's SHA-256.
Both runs retain `instrumentation.txt` and `logcat.txt` for diagnosis.
