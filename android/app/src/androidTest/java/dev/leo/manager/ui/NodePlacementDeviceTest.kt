package dev.leo.manager.ui

import android.graphics.Bitmap
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NodePlacementDeviceTest : NodePlacementCases() {
    override fun capture(name: String) {
        compose.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val directory =
            File(instrumentation.targetContext.filesDir, "node-screenshots").apply { mkdirs() }
        requireNotNull(instrumentation.uiAutomation.takeScreenshot()).let { bitmap ->
            File(directory, "$name.png").outputStream().use {
                bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
            }
            bitmap.recycle()
        }
    }
}
