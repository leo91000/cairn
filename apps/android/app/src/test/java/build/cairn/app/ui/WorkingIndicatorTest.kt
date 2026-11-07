package build.cairn.app.ui

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
import build.cairn.app.data.RunEvent
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
            id = id,
            createdAt = 1000 + id,
            type = if (running) "item.started" else "item.completed",
            text = "",
            payload =
                kotlinx.serialization.json.JsonObject(
                    mapOf(
                        "item" to
                            buildJsonObject {
                                put("type", "command_execution")
                                put("command", command)
                                if (running) put("status", "in_progress")
                                else put("exit_code", exit)
                            }
                    )
                ),
        )

    private fun user(id: Long, at: Long) =
        RunEvent(
            id = id,
            createdAt = at,
            type = "chat.user",
            text = "Vérifie les tests",
        )

    @Test
    fun `the step in progress is named with its command`() {
        val step =
            workingStep(
                listOf(
                    user(1, 5000),
                    tool(2, "cat README.md"),
                    tool(3, "./gradlew testDebugUnitTest", running = true),
                ),
                "Cairn",
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
            workingStep(listOf(user(1, 5000), tool(2, "sed -n 1,20p ui/Signal.kt")), "Cairn", null)
        assertEquals("Cairn travaille", step.title)
        assertEquals("Dernière étape : Lire Signal.kt", step.detail)
    }

    @Test
    fun `steps from an earlier turn and session notices are ignored`() {
        val events =
            listOf(
                tool(1, "./gradlew build", running = true),
                user(2, 7000),
                RunEvent(
                    id = 3,
                    createdAt = 7100,
                    type = "turn.started",
                    text = """{"type":"turn.started"}""",
                ),
            )
        val step = workingStep(events, "", 3000)
        assertEquals("L’agent travaille", step.title)
        assertEquals("", step.detail)
        assertEquals(7000L, step.since)
        // Without any message, the run start is used.
        assertEquals(3000L, workingStep(emptyList(), "Cairn", 3000).since)
    }

    @Test
    fun `a failed step is not presented as running`() {
        val step = workingStep(listOf(user(1, 1), tool(2, "cargo test", exit = 101)), "Cairn", null)
        assertEquals("Cairn travaille", step.title)
        assertEquals("Dernière étape : Exécuter les tests", step.detail)
    }

    @Test
    fun `presents the server background wait and local fallback labels`() {
        val notice =
            build.cairn.contracts.TimelineNotice(
                code = "backgroundTasks",
                state = "info",
                prominent = false,
                tasks = listOf("Build", " "),
                taskIds = listOf("one", "two"),
                waitingSince = 1000,
            )
        val card =
            build.cairn.app.data.TimelineCard(
                id = "notice:3",
                runId = "run",
                position = 3,
                revision = 3,
                source = 3,
                turn = 1,
                attempt = 0,
                createdAt = 9000,
                updatedAt = 9000,
                content = build.cairn.contracts.TimelineContent.C(notice),
            )
        val wait = backgroundWait(listOf(card))
        assertEquals(
            BackgroundWait(listOf("Build", "Tâche en arrière-plan"), 1000, listOf("one", "two")),
            wait,
        )
        assertEquals("En attente de 2 tâches en arrière-plan", waitingStep(wait!!).title)
        assertNull(
            backgroundWait(
                listOf(
                    card.copy(
                        content =
                            build.cairn.contracts.TimelineContent.C(
                                notice.copy(waitingSince = null)
                            )
                    )
                )
            )
        )
        assertNull(backgroundWait(emptyList()))
    }

    @Test
    fun `the background wait lists its first tasks and announces when the agent resumes`() {
        var dark by androidx.compose.runtime.mutableStateOf(false)
        val started = System.currentTimeMillis() - 725_000
        compose.mainClock.autoAdvance = false
        compose.setContent {
            CairnTheme(if (dark) "dark" else "light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(20.dp)) {
                    WorkingIndicator(
                        waitingStep(BackgroundWait(listOf("Wait for PR 77 CI"), started))
                    )
                    Spacer(Modifier.height(20.dp))
                    WorkingIndicator(
                        waitingStep(
                            BackgroundWait(
                                listOf("Build", "Watch CI", "Rebuild the index", "Deploy"),
                                started,
                            )
                        )
                    )
                }
            }
        }
        compose.mainClock.advanceTimeBy(700)
        compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
        val single = compose.onAllNodesWithTag("agent-waiting")[0]
        single.assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.ContentDescription,
                listOf(
                    "En attente d’une tâche en arrière-plan : Wait for PR 77 CI. L’agent reprend quand elle se termine."
                ),
            )
        )
        single.assert(SemanticsMatcher.keyIsDefined(SemanticsProperties.LiveRegion))
        compose
            .onAllNodesWithTag("agent-waiting")[1]
            .assert(
                SemanticsMatcher.expectValue(
                    SemanticsProperties.ContentDescription,
                    listOf(
                        "En attente de 4 tâches en arrière-plan : Build · Watch CI · Rebuild the index · Deploy. L’agent reprend quand elles se terminent."
                    ),
                )
            )
        // Three tasks at most, then a count of the others.
        compose.onNodeWithText("+1", useUnmergedTree = true).assertExists()
        compose.onNodeWithText("Deploy", useUnmergedTree = true).assertDoesNotExist()
        // The time counts from the start of the wait.
        compose
            .onAllNodesWithText("12 min", substring = true, useUnmergedTree = true)
            .assertCountEquals(2)
        capture("waiting-light")
        compose.runOnIdle { dark = true }
        compose.mainClock.advanceTimeBy(700)
        capture("waiting-dark")
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
            CairnTheme(if (dark) "dark" else "light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(20.dp)) {
                    WorkingIndicator(
                        WorkingStep("Exécuter les tests", "./gradlew testDebugUnitTest", started)
                    )
                    Spacer(Modifier.height(20.dp))
                    WorkingIndicator(
                        WorkingStep("Cairn travaille", "Dernière étape : Lire Signal.kt", started)
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
            CairnTheme(if (dark) "dark" else "light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(20.dp)) {
                    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                        WorkingAvatar("Cairn", "cairn", 40.dp)
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
            CairnTheme("light") {
                Box(Modifier.background(MaterialTheme.colorScheme.background).padding(24.dp)) {
                    WorkingAvatar("Cairn", "cairn", 96.dp)
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
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
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
