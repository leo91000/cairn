package dev.leo.manager.ui

import android.app.Application
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
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
        LeoViewModel(ApplicationProvider.getApplicationContext<Application>(), officialOrigin = "")

    private val workspace =
        Workspace(
            installation = Installation("fixture", "Installation de test", "owner", true),
            ready = true,
            agents = listOf(Agent(id = "agent", name = "Reviewer")),
            projects = listOf(Project(id = "project", name = "Leo Agent Manager")),
        )

    @Test
    fun `task form prevents empty saves and restores drafts after recreation`() {
        val vm = model()
        val restoration = StateRestorationTester(compose)
        restoration.setContent {
            LeoTheme {
                TaskEditor(vm, workspace, Task(agentId = "agent", projectId = "project")) {}
            }
        }
        compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
        compose.onNodeWithText("Nom").performTextInput("Revue du projet")
        compose
            .onNodeWithText("Mission et critères de réussite")
            .performTextInput("Examiner les changements et lancer les vérifications.")
        compose.onNodeWithText("Enregistrer").assertIsEnabled()
        restoration.emulateSavedInstanceStateRestore()
        compose.onNodeWithText("Revue du projet").assertExists()
        compose
            .onNodeWithText("Examiner les changements et lancer les vérifications.")
            .assertExists()
        compose.onNodeWithText("Enregistrer").assertIsEnabled()
        screenshot("task-editor")
    }

    @Test
    fun `mission filters distinguish archived missions and prevent running them`() {
        val vm = model()
        compose.setContent {
            LeoTheme {
                MissionsScreen(
                    vm,
                    workspace.copy(
                        tasks =
                            listOf(
                                Task(
                                    id = "a",
                                    name = "Revue quotidienne",
                                    agentId = "agent",
                                    projectId = "project",
                                    cron = "0 9 * * *",
                                    nextRun = 1789405200000,
                                ),
                                Task(
                                    id = "b",
                                    name = "Ancienne mission",
                                    agentId = "agent",
                                    projectId = "project",
                                    archived = true,
                                    enabled = false,
                                ),
                            )
                    ),
                ) {}
            }
        }
        compose.onAllNodesWithText("Revue quotidienne").onFirst().assertExists()
        compose.onNodeWithText("Ancienne mission").assertDoesNotExist()
        // The daily cron is described in words on the card.
        compose.onNodeWithText("Tous les jours · 09:00").assertExists()
        screenshot("tasks")
        compose.onNodeWithText("Archivées").performScrollTo().performClick()
        compose.onAllNodesWithText("Ancienne mission").onFirst().assertExists()
        compose.onNodeWithText("Revue quotidienne").assertDoesNotExist()
        compose.onNodeWithContentDescription("Lancer Ancienne mission").assertIsNotEnabled()
        compose.onAllNodesWithText("Ancienne mission").onFirst().performClick()
        compose.onNodeWithTag("mission-sheet").assertIsDisplayed()
        compose.onNodeWithText("Lancer maintenant").assertIsNotEnabled()
    }

    @Test
    fun `run card exposes its real status and navigates with the run ID`() {
        var opened = ""
        compose.setContent {
            LeoTheme {
                RunCard(Run(id = "run-1", status = "failed", taskName = "Revue du projet")) {
                    opened = it
                }
            }
        }
        compose.onNodeWithText("Échec").assertExists()
        compose.onNodeWithText("Revue du projet").performClick()
        assertTrue(opened == "run-1")
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
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
