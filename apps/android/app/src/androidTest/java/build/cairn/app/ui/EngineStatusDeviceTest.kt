package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RobolectricTwin
@RunWith(AndroidJUnit4::class)
class EngineStatusDeviceTest : EngineStatusCases() {
    override fun capture(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        instrumentation.waitForIdleSync()
        val directory =
            File(instrumentation.targetContext.filesDir, "engine-status").apply { mkdirs() }
        File(directory, "$name.png").outputStream().use {
            check(
                instrumentation.uiAutomation
                    .takeScreenshot()
                    .compress(Bitmap.CompressFormat.PNG, 100, it)
            )
        }
    }
}
