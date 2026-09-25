package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.RunEvent
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class WorkingIndicatorTest {
    @get:Rule val compose = createComposeRule()

    private fun tool(id: Long, command: String, running: Boolean = false, exit: Int = 0) =
        RunEvent(
            id,
            1000 + id,
            if (running) "item.started" else "item.completed",
            "",
            mapOf(
                "item" to
                    buildJsonObject {
                        put("type", "command_execution")
                        put("command", command)
                        if (running) put("status", "in_progress") else put("exit_code", exit)
                    }
            ),
        )

    private fun user(id: Long, at: Long) = RunEvent(id, at, "chat.user", "Vérifie les tests")

    @Test
    fun `the step in progress is named with its command`() {
        val step =
            workingStep(
                listOf(
                    user(1, 5000),
                    tool(2, "cat README.md"),
                    tool(3, "./gradlew testDebugUnitTest", running = true),
                ),
                "Leo",
                10,
            )
        assertEquals("Exécuter les tests", step.title)
        assertEquals("./gradlew testDebugUnitTest", step.detail)
        // Time runs from the latest message, not from the start of the conversation run.
        assertEquals(5000L, step.since)
    }

    @Test
    fun `without a running step the agent works and shows its last step`() {
        val step =
            workingStep(listOf(user(1, 5000), tool(2, "sed -n 1,20p ui/Signal.kt")), "Leo", null)
        assertEquals("Leo travaille", step.title)
        assertEquals("Dernière étape : Lire Signal.kt", step.detail)
    }

    @Test
    fun `steps from an earlier turn and session notices are ignored`() {
        val events =
            listOf(
                tool(1, "./gradlew build", running = true),
                user(2, 7000),
                RunEvent(3, 7100, "turn.started", """{"type":"turn.started"}"""),
            )
        val step = workingStep(events, "", 3000)
        assertEquals("L’agent travaille", step.title)
        assertEquals("", step.detail)
        assertEquals(7000L, step.since)
        // Without any message, the run start is used.
        assertEquals(3000L, workingStep(emptyList(), "Leo", 3000).since)
    }

    @Test
    fun `a failed step is not presented as running`() {
        val step = workingStep(listOf(user(1, 1), tool(2, "cargo test", exit = 101)), "Leo", null)
        assertEquals("Leo travaille", step.title)
        assertEquals("Dernière étape : Exécuter les tests", step.detail)
    }

    @Test
    fun `elapsed time is precise to the second`() {
        assertEquals("45 s", liveElapsed(0 + 1, 45_001))
        assertEquals("2 min 04 s", liveElapsed(1, 124_001))
        assertEquals("1 h 05", liveElapsed(1, 3_900_001))
        assertEquals("", liveElapsed(null, 10))
    }

    @Test
    fun `indicator announces the step and renders in both themes`() {
        var dark by androidx.compose.runtime.mutableStateOf(false)
        val started = System.currentTimeMillis() - 134_000
        compose.mainClock.autoAdvance = false
        compose.setContent {
            LeoTheme(if (dark) "dark" else "light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(20.dp)) {
                    WorkingIndicator(
                        WorkingStep("Exécuter les tests", "./gradlew testDebugUnitTest", started)
                    )
                    Spacer(Modifier.height(20.dp))
                    WorkingIndicator(
                        WorkingStep("Leo travaille", "Dernière étape : Lire Signal.kt", started)
                    )
                }
            }
        }
        compose.mainClock.advanceTimeBy(700)
        val node = compose.onAllNodesWithTag("agent-working")[0]
        node.assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.ContentDescription,
                listOf("Exécuter les tests : ./gradlew testDebugUnitTest"),
            )
        )
        node.assert(SemanticsMatcher.keyIsDefined(SemanticsProperties.LiveRegion))
        compose.onAllNodesWithTag("agent-working").assertCountEquals(2)
        capture("working-light")
        compose.runOnIdle { dark = true }
        compose.mainClock.advanceTimeBy(700)
        capture("working-dark")
    }

    @Test
    fun `working conversations animate their avatar and status in the list`() {
        var dark by androidx.compose.runtime.mutableStateOf(false)
        compose.mainClock.autoAdvance = false
        compose.setContent {
            LeoTheme(if (dark) "dark" else "light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(20.dp)) {
                    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                        WorkingAvatar("Leo", "leo", 40.dp)
                        Spacer(Modifier.width(12.dp))
                        WorkingLabel("En cours")
                    }
                    Spacer(Modifier.height(16.dp))
                    Row { AgentAvatar("Reviewer", "reviewer", 40.dp) }
                }
            }
        }
        compose.mainClock.advanceTimeBy(500)
        compose.onNodeWithTag("chat-working-avatar").assertWidthIsEqualTo(40.dp)
        compose.onNodeWithText("En cours").assertExists()
        capture("working-list-light")
        compose.runOnIdle { dark = true }
        compose.mainClock.advanceTimeBy(900)
        capture("working-list-dark")
    }

    @Test
    fun `the comet travels along the avatar edge while the frame stays still`() {
        compose.mainClock.autoAdvance = false
        compose.setContent {
            LeoTheme("light") {
                Box(Modifier.background(MaterialTheme.colorScheme.background).padding(24.dp)) {
                    WorkingAvatar("Leo", "leo", 96.dp)
                }
            }
        }
        // One orbit takes 1.8 s; eight frames show the head going round the rounded frame.
        repeat(8) { frame ->
            compose.mainClock.advanceTimeBy(225)
            capture("comet-$frame")
        }
        compose.onNodeWithTag("chat-working-avatar").assertWidthIsEqualTo(96.dp)
    }

    private fun capture(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
        File(dir).mkdirs()
        compose
            .onRoot()
            .captureToImage()
            .asAndroidBitmap()
            .compress(
                android.graphics.Bitmap.CompressFormat.PNG,
                100,
                File(dir, "$name.png").outputStream(),
            )
    }
}
