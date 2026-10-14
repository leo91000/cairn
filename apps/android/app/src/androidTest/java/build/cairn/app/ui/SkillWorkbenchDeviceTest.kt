package build.cairn.app.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

/** Real rendering of the skill workbench journeys; screenshots land in `skill-workbench/`. */
@RunWith(AndroidJUnit4::class)
class SkillWorkbenchDeviceTest : SkillWorkbenchCases() {
    override fun capture(name: String) {
        compose.waitForIdle()
        // Let sheets and async image decoding settle before the frame is taken.
        Thread.sleep(600)
        compose.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val directory =
            File(instrumentation.targetContext.filesDir, "skill-workbench").apply { mkdirs() }
        requireNotNull(instrumentation.uiAutomation.takeScreenshot()).let { bitmap ->
            File(directory, "$name.png").outputStream().use {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            bitmap.recycle()
        }
    }
}
