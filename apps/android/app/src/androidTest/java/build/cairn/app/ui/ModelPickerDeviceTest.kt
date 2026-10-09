package build.cairn.app.ui

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ModelPickerDeviceTest : ModelPickerCases() {
    override fun captureReasoning() {
        compose.waitForIdle()
        val instrumentation =
            androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        val file =
            java.io.File(instrumentation.targetContext.filesDir, "model-picker/reasoning.png")
        file.parentFile!!.mkdirs()
        val bitmap = checkNotNull(instrumentation.uiAutomation.takeScreenshot())
        file.outputStream().use {
            bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
        bitmap.recycle()
    }
}
