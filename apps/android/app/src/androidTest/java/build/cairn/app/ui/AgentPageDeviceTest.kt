package build.cairn.app.ui

import android.content.res.Configuration
import android.graphics.Bitmap
import androidx.core.view.WindowCompat
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry
import androidx.test.runner.lifecycle.Stage
import java.io.File
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AgentPageDeviceTest : AgentPageCases() {
    @Test
    fun contributionGrantUsesResourceAncestryAndAnExplicitCredentialSupplier() {
        runContributionGrantSelectionJourney()
    }

    override fun capture(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        // The test activity does not style its bars like MainActivity's edge-to-edge setup:
        // give the status bar the same icon appearance so captures read like the app.
        instrumentation.runOnMainSync {
            ActivityLifecycleMonitorRegistry.getInstance()
                .getActivitiesInStage(Stage.RESUMED)
                .forEach { activity ->
                    val dark =
                        activity.resources.configuration.uiMode and
                            Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
                    WindowCompat.getInsetsController(activity.window, activity.window.decorView)
                        .apply {
                            isAppearanceLightStatusBars = !dark
                            isAppearanceLightNavigationBars = !dark
                        }
                }
        }
        compose.waitForIdle()
        // Let dialog, ripple and status bar icon animations settle before reading the screen.
        Thread.sleep(800)
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
