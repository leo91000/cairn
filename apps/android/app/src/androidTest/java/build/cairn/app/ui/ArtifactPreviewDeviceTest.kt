package build.cairn.app.ui

import android.app.Application
import android.graphics.Color
import androidx.activity.ComponentActivity
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performTouchInput
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.data.CairnViewModel
import java.io.File
import java.util.concurrent.TimeUnit
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Real WebView decoding and resource blocking, which Robolectric cannot exercise. */
@RunWith(AndroidJUnit4::class)
class ArtifactPreviewDeviceTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun gifPreviewAnimatesInTheFileViewer() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val application = ApplicationProvider.getApplicationContext<Application>()
        val file = File(application.cacheDir, "device-preview.gif")
        instrumentation.context.assets.open("animated.gif").use { input ->
            file.outputStream().use { input.copyTo(it) }
        }
        val vm =
            ViewModelProvider(
                compose.activity,
                viewModelFactory { initializer { CairnViewModel(application, beaconOrigin = "") } },
            )[CairnViewModel::class.java]
        try {
            compose.setContent {
                CairnTheme("light") {
                    FilePreview(vm, "", "animated.gif", "image/gif", "image", file.path) {}
                }
            }
            compose.waitUntil(15000) { screenContains(Color.RED) }
            compose.waitUntil(15000) { screenContains(Color.BLUE) }
            val originalPixels = screenColorCount(Color.RED, Color.BLUE)
            compose.onNodeWithTag("artifact-image-gestures").performTouchInput {
                pinch(
                    start0 = Offset(width * .4f, centerY),
                    end0 = Offset(width * .15f, centerY),
                    start1 = Offset(width * .6f, centerY),
                    end1 = Offset(width * .85f, centerY),
                    durationMillis = 500,
                )
            }
            compose.waitUntil(15000) {
                screenColorCount(Color.RED, Color.BLUE) > originalPixels * 2
            }
        } finally {
            file.delete()
        }
    }

    @Test
    fun htmlRendersWithoutExecutingScriptsOrLoadingNetworkResources() {
        MockWebServer().use { server ->
            server.start()
            compose.setContent {
                InertWebPage(
                    "<body style='margin:0;background:lime'>" +
                        "<script>document.body.style.background='red'</script>" +
                        "<img src='${server.url("/tracker")}'><h1>Offline preview</h1>",
                    "preview.html",
                )
            }
            compose.waitUntil(15000) { screenContains(Color.GREEN) }
            assertNull(server.takeRequest(1, TimeUnit.SECONDS))
            assertFalse(screenContains(Color.RED))
        }
    }

    private fun screenContains(color: Int): Boolean = screenColorCount(color) > 20

    private fun screenColorCount(vararg colors: Int): Int {
        val bitmap =
            InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot() ?: return 0
        try {
            var matches = 0
            for (y in 0 until bitmap.height step 4) for (x in 0 until bitmap.width step 4) if (
                bitmap.getPixel(x, y) in colors
            )
                matches++
            return matches
        } finally {
            bitmap.recycle()
        }
    }
}
