package dev.leo.manager.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class QueueStripDeviceTest : QueueStripCases() {
    override fun capture(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        // Semantics can be ready before the frame reaches the screen.
        compose.waitForIdle()
        instrumentation.waitForIdleSync()
        android.os.SystemClock.sleep(500)
        val directory =
            File(instrumentation.targetContext.filesDir, "queue-strip").apply { mkdirs() }
        File(directory, "$name.png").outputStream().use {
            check(
                instrumentation.uiAutomation
                    .takeScreenshot()
                    .compress(Bitmap.CompressFormat.PNG, 100, it)
            )
        }
    }
}
