package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.RunEvent
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class AgentStepsTest {
    @get:Rule val compose = createComposeRule()

    private fun item(
        id: Long,
        type: String,
        started: Boolean = false,
        body: JsonObjectBuilder.() -> Unit,
    ) =
        RunEvent(
            id,
            1000 + id,
            if (started) "item.started" else "item.completed",
            "",
            mapOf(
                "item" to
                    buildJsonObject {
                        put("type", type)
                        put("id", "i$id")
                        body()
                    }
            ),
        )

    private fun command(id: Long, command: String, exit: Int = 0, output: String = "") =
        item(id, "command_execution") {
            put("command", command)
            put("exit_code", exit)
            if (output.isNotEmpty()) put("aggregated_output", output)
        }

    private val events =
        listOf(
            item(1, "reasoning") { put("text", "Je regarde comment l’en-tête est construit.") },
            command(2, "sed -n 1,80p android/app/src/main/java/dev/leo/manager/ui/ChatsScreen.kt"),
            command(3, "sed -n 1,60p android/app/src/main/java/dev/leo/manager/ui/Signal.kt"),
            command(4, "rg -n \"ConversationHeader\" android/app/src"),
            item(5, "file_change") {
                put(
                    "changes",
                    buildJsonArray {
                        add(
                            buildJsonObject {
                                put("path", "ui/ConversationPresentation.kt")
                                put("kind", "update")
                            }
                        )
                        add(
                            buildJsonObject {
                                put("path", "ui/ChatsScreen.kt")
                                put("kind", "update")
                            }
                        )
                    },
                )
            },
            command(
                6,
                "./gradlew testDebugUnitTest",
                exit = 1,
                output = "ChatJourneyTest > header FAILED\n1 failed",
            ),
            item(7, "command_execution", started = true) {
                put("command", "./gradlew testDebugUnitTest")
                put("status", "in_progress")
            },
        )

    private val presentations = events.map(::presentActivity)

    @Test
    fun `the sentence summarises finished actions by kind`() {
        val finished = presentations.filter { !it.isRunning() }
        assertEquals(
            "A lu 2 fichiers, cherché 1 fois, modifié 2 fichiers, lancé 1 commande",
            actionSentence(finished),
        )
        assertEquals("A réfléchi", actionSentence(presentations.take(1)))
        val notice = presentActivity(RunEvent(1, 1, "run.started", "Le worker démarre"))
        assertEquals("Suivi de l’exécution", actionSentence(listOf(notice)))
    }

    @Test
    fun `consecutive reads fold into one step and edits name their files`() {
        val steps = agentSteps(presentations)
        assertEquals(
            listOf(
                "Réflexion",
                "Lire 2 fichiers",
                "Rechercher dans les fichiers",
                "Fichiers modifiés",
                "Exécuter les tests",
                "Exécuter les tests",
            ),
            steps.map { it.title },
        )
        assertEquals("ChatsScreen.kt · Signal.kt", steps[1].detail)
        assertEquals(2, steps[1].items.size)
        assertEquals("ConversationPresentation.kt · ChatsScreen.kt", steps[3].detail)
        assertTrue(steps[4].failed)
        assertTrue(steps[5].running)
        // A failed read stays on its own line.
        val failedRead = presentActivity(command(9, "cat missing.md", exit = 1))
        assertEquals(2, agentSteps(listOf(presentations[1], failedRead)).size)
    }

    @Test
    fun `actions expand into a timeline and open the full detail of a step`() {
        var dark by mutableStateOf(false)
        compose.setContent {
            LeoTheme(if (dark) "dark" else "light") {
                Column(
                    Modifier.fillMaxSize()
                        .background(MaterialTheme.colorScheme.background)
                        .padding(16.dp)
                ) {
                    AgentActions("activity:1", presentations, hideRunning = true)
                }
            }
        }
        compose
            .onNodeWithTag("agent-actions")
            .assert(
                hasText("A lu 2 fichiers, cherché 1 fois, modifié 2 fichiers, lancé 1 commande")
            )
            .assert(hasText("5 étapes"))
            .assert(hasText(" 1 échec"))
        capture("agent-actions-collapsed")
        compose.onNodeWithTag("agent-actions").performClick()
        // The running command is left to the working indicator.
        compose.onAllNodesWithTag("agent-step").assertCountEquals(5)
        compose.onNodeWithText("Lire 2 fichiers").assertIsDisplayed()
        compose.onNodeWithText("Code 1").assertIsDisplayed()
        capture("agent-actions-expanded")
        compose.onNodeWithText("Code 1").performClick()
        compose.onNodeWithTag("agent-step-sheet").assertExists()
        compose.onNodeWithText("Échec · code 1 · étape 5 sur 5").assertExists()
        compose.onNodeWithText("Copier la sortie").assertExists()
        capture("agent-step-sheet")
        compose.onNodeWithContentDescription("Fermer").performClick()
        compose.waitUntil(10000) {
            compose.onAllNodesWithTag("agent-step-sheet").fetchSemanticsNodes().isEmpty()
        }
        compose.runOnIdle { dark = true }
        compose.waitForIdle()
        capture("agent-actions-expanded-dark")
        compose.onNodeWithTag("agent-actions").performClick()
        compose.onAllNodesWithTag("agent-step").assertCountEquals(0)
    }

    @Test
    fun `a group with only the running step shows nothing while the agent works`() {
        compose.setContent {
            LeoTheme { AgentActions("activity:7", presentations.takeLast(1), hideRunning = true) }
        }
        compose.onNodeWithTag("agent-actions").assertDoesNotExist()
    }

    private fun capture(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        val target =
            if (compose.onAllNodes(isDialog()).fetchSemanticsNodes().isNotEmpty())
                compose.onNode(isDialog())
            else compose.onRoot()
        target
            .captureToImage()
            .asAndroidBitmap()
            .compress(
                android.graphics.Bitmap.CompressFormat.PNG,
                100,
                File(dir, "$name.png").outputStream(),
            )
    }
}
