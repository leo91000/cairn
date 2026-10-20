package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AgentPageDeviceTest : AgentPageCases() {
    override fun capture(name: String) {
        compose.waitForIdle()
        // Let dialog and ripple animations settle before reading the screen.
        Thread.sleep(800)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val directory =
            File(instrumentation.targetContext.filesDir, "agent-screenshots").apply { mkdirs() }
        requireNotNull(instrumentation.uiAutomation.takeScreenshot()).let { bitmap ->
            File(directory, "$name.png").outputStream().use {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            bitmap.recycle()
        }
    }
}
