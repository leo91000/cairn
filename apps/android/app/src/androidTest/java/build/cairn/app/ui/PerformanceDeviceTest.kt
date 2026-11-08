package build.cairn.app.ui

import android.content.ClipboardManager
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.data.wireJson
import build.cairn.app.diagnostics.*
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PerformanceDeviceTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun nativeFramesAreCollectedAndTheReportCanBeCopiedWithoutConversationData() {
        Performance.store.clear()
        Performance.screen = PerfScreen.SETTINGS
        var counter by mutableIntStateOf(0)
        var telemetryEnabled by mutableStateOf(true)
        val monitor = FrameMonitor(compose.activity.window)
        compose.runOnUiThread { monitor.start() }
        compose.setContent {
            CairnTheme {
                Column(
                    Modifier.fillMaxSize().systemBarsPadding().verticalScroll(rememberScrollState())
                ) {
                    Button(onClick = { counter++ }) { Text("Dessiner $counter") }
                    PerformanceSettings(telemetryEnabled) { telemetryEnabled = it }
                }
            }
        }
        try {
            compose.onNodeWithText("Dessiner 0").performClick()
            compose.waitUntil(10_000) { Performance.store.snapshot().frames.sumOf { it.count } > 0 }
            compose.onNodeWithText("Actualiser les mesures").performScrollTo().performClick()
            compose
                .onNodeWithText("Copier le rapport de performances")
                .performScrollTo()
                .performClick()
            val clipboard = compose.activity.getSystemService(ClipboardManager::class.java)
            var report = ""
            compose.runOnUiThread { report = clipboard.primaryClip!!.getItemAt(0).text.toString() }
            val json = wireJson.parseToJsonElement(report).jsonObject
            assertEquals(1, json.getValue("schema").jsonPrimitive.int)
            assertTrue(
                json.getValue("measurements").jsonObject.getValue("frames").jsonArray.isNotEmpty()
            )
            assertFalse(report.contains("cairn_session"))
            assertFalse(report.contains("/api/"))
            assertFalse(report.contains("csrf"))
            compose
                .onNodeWithText("Envoyer les diagnostics de performances")
                .performScrollTo()
                .assertIsOn()
                .performClick()
                .assertIsOff()
            val context = InstrumentationRegistry.getInstrumentation().targetContext
            val dir = File(context.filesDir, "performance-validation").also { it.mkdirs() }
            File(dir, "report.json").writeText(report)
            InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot().let { bitmap
                ->
                File(dir, "settings.png").outputStream().use {
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
                }
            }
            // Other work may record measurements at any time, so the reset is checked against
            // a ten-minute operation and frame that nothing else in the app produces.
            val markerNs = 600_000_000_000L
            val markerMs = markerNs / 1e6
            Performance.store.timing(PerfOperation.API_WRITE, PerfScreen.STARTUP, markerNs, false)
            Performance.store.frame(PerfScreen.STARTUP, true, markerNs, 16_000_000)
            val beforeReset = Performance.store.snapshot()
            assertTrue(beforeReset.operations.any { it.maxMs >= markerMs })
            assertTrue(beforeReset.frames.any { it.maxMs >= markerMs })
            compose.onNodeWithText("Remettre à zéro").performScrollTo().performClick()
            val afterReset = Performance.store.snapshot()
            assertNotEquals(beforeReset.windowId, afterReset.windowId)
            assertTrue(afterReset.operations.none { it.maxMs >= markerMs })
            assertTrue(afterReset.frames.none { it.maxMs >= markerMs })
            assertTrue(afterReset.recentSlowSamples.none { it.durationMs >= markerMs })
        } finally {
            compose.runOnUiThread { monitor.stop() }
        }
    }
}
