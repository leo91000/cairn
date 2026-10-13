package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RobolectricTwin
@RunWith(AndroidJUnit4::class)
class UnifiedHeaderDeviceTest : UnifiedHeaderCases() {
    override fun capture(name: String) {
        compose.waitForIdle()
        // Emulator frames can reach the screen after Compose reports idle.
        Thread.sleep(2000)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val directory =
            File(instrumentation.targetContext.filesDir, "header-screenshots").apply { mkdirs() }
        requireNotNull(instrumentation.uiAutomation.takeScreenshot()).let { bitmap ->
            File(directory, "$name.png").outputStream().use {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            bitmap.recycle()
        }
    }
}
