package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RobolectricTwin
@RunWith(AndroidJUnit4::class)
class PluginsDeviceTest : PluginsCases() {
    override fun capture(name: String) {
        compose.waitForIdle()
        // Let the render thread draw the last composed frame before the system screenshot.
        android.os.SystemClock.sleep(700)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val expectedPackage = instrumentation.targetContext.packageName
        fun activePackage(): String {
            var packageName: String? = null
            compose.waitUntil(timeoutMillis = 2_000) {
                packageName =
                    instrumentation.uiAutomation.rootInActiveWindow?.packageName?.toString()
                packageName != null
            }
            return requireNotNull(packageName)
        }

        val beforePackage = activePackage()
        val directory =
            File(instrumentation.targetContext.filesDir, "plugin-screenshots").apply { mkdirs() }
        requireNotNull(instrumentation.uiAutomation.takeScreenshot()).let { bitmap ->
            File(directory, "$name.png").outputStream().use {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            bitmap.recycle()
        }
        val afterPackage = activePackage()
        check(beforePackage == expectedPackage && afterPackage == expectedPackage) {
            "Capture $name is obscured by another window: $beforePackage -> $afterPackage"
        }
    }
}
