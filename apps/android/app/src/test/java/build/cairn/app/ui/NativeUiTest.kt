package build.cairn.app.ui

import android.app.Application
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NativeUiTest {
    @get:Rule val compose = createComposeRule()

    private fun model() =
        CairnViewModel(
            ApplicationProvider.getApplicationContext<Application>(),
            beaconOrigin = "",
        )

    private val workspace =
        Workspace(
            installation =
                Installation("fixture", "Installation de test", InstallationRole.Owner, true),
            ready = true,
            agents =
                listOf(
                    Agent(
                        exclusiveNodes = emptyList(),
                        plugins = emptyList(),
                        revision = "",
                        createdAt = 0,
                        access =
                            AccessPolicy(
                                mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                projects = null,
                                skills = null,
                                mcps = null,
                            ),
                        id = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
                        name = "Reviewer",
                    )
                ),
            projects = listOf(Project(id = "46d045dd-9bce-51e3-8a99-65aa9384efc9", name = "Cairn")),
        )

    @Test
    fun `run card exposes its real status and navigates with the run ID`() {
        var opened = ""
        compose.setContent {
            CairnTheme {
                RunCard(
                    Run(
                        id = "run-1",
                        status = "failed",
                        taskName = "Revue du projet",
                        taskId = "",
                    )
                ) {
                    opened = it
                }
            }
        }
        compose.onNodeWithText("Échec").assertExists()
        compose.onNodeWithText("Revue du projet").performClick()
        assertTrue(opened == "run-1")
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        (if (name == "task-editor") compose.onNode(isDialog()) else compose.onRoot())
            .captureToImage()
            .asAndroidBitmap()
            .let { bitmap ->
                File(dir, "$name.png").outputStream().use {
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
                }
            }
    }
}
