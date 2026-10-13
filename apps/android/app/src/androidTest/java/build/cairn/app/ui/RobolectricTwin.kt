package build.cairn.app.ui

/**
 * The same journey already runs under Robolectric in `testDebugUnitTest`, and the device class only
 * adds screenshots. Pull requests skip these classes on the emulator; main, tags and manual runs
 * execute every device test. Leave it off when the device run checks something Robolectric cannot,
 * such as the Android Keystore, real gestures or system permissions.
 */
@Target(AnnotationTarget.CLASS)
@Retention(AnnotationRetention.RUNTIME)
annotation class RobolectricTwin
