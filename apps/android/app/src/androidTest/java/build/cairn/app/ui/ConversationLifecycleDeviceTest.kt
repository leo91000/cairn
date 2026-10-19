package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RobolectricTwin
@RunWith(AndroidJUnit4::class)
class ConversationLifecycleDeviceTest : ConversationLifecycleCases() {
    override fun captureFreshSession(name: String) = capture("fresh-session-$name.png")

    override fun captureSwipe() = capture("swipe-fil.png")

    private fun capture(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        // The mid-gesture frame must reach the display before the system screenshot.
        instrumentation.waitForIdleSync()
        Thread.sleep(500)
        val directory =
            File(instrumentation.targetContext.filesDir, "conversation-lifecycle").apply {
                mkdirs()
            }
        File(directory, name).outputStream().use {
            check(
                instrumentation.uiAutomation
                    .takeScreenshot()
                    .compress(Bitmap.CompressFormat.PNG, 100, it)
            )
        }
    }
}
