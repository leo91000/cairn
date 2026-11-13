package build.cairn.app.ui

import android.app.Application
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.*
import java.io.File
import org.junit.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode
import org.robolectric.shadows.ShadowLooper

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NodePlacementTest : NodePlacementCases() {
    override fun capture(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        val dialogs = compose.onAllNodes(isDialog()).fetchSemanticsNodes()
        val root =
            if (dialogs.isEmpty()) compose.onRoot() else compose.onAllNodes(isDialog()).onLast()
        root.captureToImage().asAndroidBitmap().let { bitmap ->
            File(dir, "$name.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }

    override fun elapse(millis: Long) =
        ShadowLooper.idleMainLooper(millis, java.util.concurrent.TimeUnit.MILLISECONDS)

    @Before
    fun setup() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun cleanup() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }
}
