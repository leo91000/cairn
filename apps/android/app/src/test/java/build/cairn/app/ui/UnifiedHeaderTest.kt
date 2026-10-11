package build.cairn.app.ui

import android.app.Application
import android.view.View
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.*
import build.cairn.app.data.NotificationPreferences
import java.io.File
import kotlin.math.roundToInt
import kotlinx.coroutines.runBlocking
import org.junit.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class UnifiedHeaderTest : UnifiedHeaderCases() {
    @Before
    fun setup() {
        val application = ApplicationProvider.getApplicationContext<Application>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            application,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        // Preferences outlive a Robolectric test: clear a push notice left by another one so
        // nothing but the header sits under the status bar.
        runBlocking { NotificationPreferences(application).setEnabled(false) }
    }

    override fun applyStatusBar(view: View) {
        val statusBars = WindowInsetsCompat.Type.statusBars()
        val height = (24 * view.resources.displayMetrics.density).roundToInt()
        ViewCompat.dispatchApplyWindowInsets(
            view,
            WindowInsetsCompat.Builder()
                .setInsets(statusBars, Insets.of(0, height, 0, 0))
                .setVisible(statusBars, true)
                .build(),
        )
    }

    @After
    fun cleanup() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    override fun capture(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        // Sheets live in their own window: capture the sheet itself when it is open.
        val sheet = compose.onAllNodesWithTag("connection-sheet").fetchSemanticsNodes()
        (if (sheet.isEmpty()) compose.onAllNodes(isRoot()).onFirst()
            else compose.onNodeWithTag("connection-sheet"))
            .captureToImage()
            .asAndroidBitmap()
            .let { bitmap ->
                File(dir, "header-$name.png").outputStream().use {
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
                }
            }
    }
}
