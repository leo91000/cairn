package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import build.cairn.app.data.RunEvent
import java.io.File
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SubagentViewsTest {
    @get:Rule val compose = createComposeRule()

    private var serial = 0L

    private fun item(type: String, body: JsonObjectBuilder.() -> Unit): RunEvent {
        val id = ++serial
        return RunEvent(
            id = id,
            createdAt = System.currentTimeMillis() - 60_000 + id * 1000,
            type = type,
            text = "",
            payload = kotlinx.serialization.json.JsonObject(mapOf("item" to buildJsonObject(body))),
            runId = "run",
        )
    }

    private fun subagent(type: String, name: String, body: JsonObjectBuilder.() -> Unit = {}) =
        item(type) {
            put("id", name)
            put("type", "subagent")
            put("name", name)
            put("agent_type", "Explore")
            put("prompt", "Audit the $name code paths and report what changed.")
            put("status", "in_progress")
            body()
        }

    private val events =
        listOf(
            subagent("item.started", "Webhooks"),
            subagent("item.started", "Billing tests"),
            item("item.completed") {
                put("id", "grep")
                put("type", "command_execution")
                put("command", "rg -n webhook crates")
                put("exit_code", 0)
                put("parent_id", "Webhooks")
            },
            item("item.started") {
                put("id", "read")
                put("type", "command_execution")
                put("command", "cat crates/api/src/webhook.rs")
                put("status", "in_progress")
                put("parent_id", "Webhooks")
            },
            item("item.completed") {
                put("id", "note")
                put("type", "subagent_message")
                put("text", "Found three handlers.")
                put("parent_id", "Webhooks")
            },
            subagent("item.completed", "Billing tests") {
                put("status", "completed")
                put("result", "All billing tests pass.")
                put("duration_ms", 4200)
                put("tokens", 1510)
            },
        )

    private val team = fixtureEntries(events, chat = true).single().agents

    @Test
    fun `a team lists its subagents and opens one's full activity`() {
        compose.setContent {
            CairnTheme("light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background).padding(16.dp)) {
                    SubagentTeam(team, active = true, identity = "team")
                }
            }
        }
        compose.onNodeWithText("2 sous-agents").assertIsDisplayed()
        compose.onNodeWithText("1 en cours").assertIsDisplayed()
        compose.onNodeWithText("1 terminé").assertIsDisplayed()
        compose.onNodeWithText("All billing tests pass.").assertIsDisplayed()
        compose.onAllNodesWithTag("subagent").assertCountEquals(2)
        capture("subagent-team")

        compose.onAllNodesWithTag("subagent")[0].performClick()
        compose.onNodeWithTag("subagent-sheet").assertIsDisplayed()
        compose.onNodeWithText("Consigne", substring = true).assertIsDisplayed()
        compose.onNodeWithText("Rechercher dans les fichiers").assertIsDisplayed()
        compose.onNodeWithText("Lire webhook.rs").assertIsDisplayed()
        compose.onNodeWithText("Dernier message").assertIsDisplayed()
        // The message itself renders through Markwon's TextView, outside Compose semantics.
        capture("subagent-sheet")

        // Subagents started together share the sheet as tabs.
        compose
            .onNode(
                hasText("Billing tests") and hasAnyAncestor(hasTestTag("subagent-sheet")),
                useUnmergedTree = true,
            )
            .performClick()
        compose.onNodeWithText("Rapport").assertIsDisplayed()
        compose.onNodeWithText("Ce sous-agent n’a signalé aucune étape.").assertIsDisplayed()
    }

    @Test
    fun `a finished team folds and its subagents show as stopped once the run ended`() {
        compose.setContent {
            CairnTheme("light") { SubagentTeam(team, active = false, identity = "team") }
        }
        compose.onNodeWithText("1 terminé").assertIsDisplayed()
        compose.onNodeWithText("1 arrêté").assertIsDisplayed()
        compose.onAllNodesWithTag("subagent").assertCountEquals(0)
        compose.onNodeWithText("2 sous-agents").performClick()
        compose.onAllNodesWithTag("subagent").assertCountEquals(2)
    }

    @Test
    fun `the bar folds into one button on phones and opens the working subagent`() {
        var opened: String? = null
        compose.setContent {
            CairnTheme("light") {
                Column(
                    Modifier.background(MaterialTheme.colorScheme.background)
                        .padding(vertical = 16.dp)
                ) {
                    SubagentBar(team, active = true) { opened = it }
                }
            }
        }
        compose.onNodeWithText("1 sous-agent").assertIsDisplayed()
        capture("subagent-bar")
        compose
            .onNodeWithContentDescription("1 sous-agent au travail. Ouvrir leur activité")
            .performClick()
        assertEquals(team.first().id, opened)
    }

    @Test
    fun `a press on the bar opens once even when a queued message moves it before release`() {
        val opened = mutableListOf<String>()
        var queued by mutableStateOf(false)
        compose.setContent {
            CairnTheme("light") {
                Column(
                    Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background),
                    verticalArrangement = Arrangement.Bottom,
                ) {
                    SubagentBar(team, active = true) { opened += it }
                    // The queue strip opens the composer below the bar and pushes it up.
                    if (queued) Spacer(Modifier.height(72.dp))
                    Spacer(Modifier.height(56.dp))
                }
            }
        }
        val button =
            compose.onNodeWithContentDescription("1 sous-agent au travail. Ouvrir leur activité")
        button.performClick()
        button.performSemanticsAction(SemanticsActions.OnClick)
        assertEquals(listOf(team.first().id, team.first().id), opened)

        val pressedTop = button.getBoundsInRoot().top
        button.performTouchInput { down(center) }
        queued = true
        compose.waitForIdle()
        assertNotEquals(pressedTop, button.getBoundsInRoot().top)
        // The finger jitters where the button was, then lifts.
        button.performTouchInput {
            moveBy(Offset(0f, 2f))
            up()
        }
        assertEquals(3, opened.size)

        // A press dragged away from both positions opens nothing.
        button.performTouchInput {
            down(center)
            moveBy(Offset(0f, -height * 4f))
            up()
        }
        assertEquals(3, opened.size)
    }

    @Test
    @Config(qualifiers = "w720dp-h900dp-xhdpi")
    fun `a chip opens from the edge of its minimum touch target`() {
        val opened = mutableListOf<String>()
        compose.setContent {
            CairnTheme("light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background)) {
                    SubagentBar(team, active = true) { opened += it }
                }
            }
        }
        // The chip draws 40dp tall inside the 48dp touch target its Surface reserves.
        val chip = compose.onAllNodesWithTag("subagent-chip")[0]
        val band = with(compose.density) { 2.dp.toPx() }
        chip.performTouchInput { click(Offset(centerX, -band)) }
        chip.performTouchInput { click(Offset(centerX, height + band)) }
        assertEquals(2, opened.size)
    }

    @Test
    @Config(qualifiers = "w720dp-h900dp-xhdpi")
    fun `swiping the chip row or stopping its fling opens no chip`() {
        val many =
            fixtureEntries(
                    (1..12).map { subagent("item.started", "Agent number $it") },
                    chat = true,
                )
                .single()
                .agents
        val opened = mutableListOf<String>()
        compose.setContent {
            CairnTheme("light") {
                Column(Modifier.background(MaterialTheme.colorScheme.background)) {
                    SubagentBar(many, active = true) { opened += it }
                }
            }
        }
        val row =
            compose.onNode(
                SemanticsMatcher.keyIsDefined(SemanticsProperties.HorizontalScrollAxisRange)
            )
        fun scrolled() =
            row.fetchSemanticsNode().config[SemanticsProperties.HorizontalScrollAxisRange].value()
        // A point on the chip nearest the row's middle, in the row's coordinates.
        fun chipNearMiddle(): Offset {
            val bounds = row.fetchSemanticsNode().boundsInRoot
            val chip =
                compose
                    .onAllNodesWithTag("subagent-chip")
                    .fetchSemanticsNodes()
                    .map { it.boundsInRoot }
                    .filter { it.width > 0f && it.left >= bounds.left && it.right <= bounds.right }
                    .minBy { kotlin.math.abs(it.center.x - bounds.center.x) }
            return Offset(chip.center.x - bounds.left, chip.center.y - bounds.top)
        }

        row.performTouchInput {
            swipeLeft(startX = right - 2f, endX = centerX, durationMillis = 400)
        }
        compose.waitForIdle()
        assertNotEquals(0f, scrolled())
        assertEquals(emptyList<String>(), opened)

        // A quick fling keeps the row moving; the tap that stops it opens nothing.
        compose.mainClock.autoAdvance = false
        row.performTouchInput {
            swipeLeft(startX = right - 2f, endX = left + 2f, durationMillis = 50)
        }
        compose.mainClock.advanceTimeBy(32)
        val flinging = scrolled()
        compose.mainClock.advanceTimeBy(32)
        assertNotEquals(flinging, scrolled())
        val tap = chipNearMiddle()
        row.performTouchInput { click(tap) }
        compose.mainClock.autoAdvance = true
        compose.waitForIdle()
        assertEquals(emptyList<String>(), opened)

        // Once the row has settled, a tap on a chip opens it.
        val settled = chipNearMiddle()
        row.performTouchInput { click(settled) }
        assertEquals(1, opened.size)
    }

    @Test
    fun `an older teammate preserves a collapsed team's scroll anchor and fold choice`() {
        val older =
            listOf(
                item("item.completed") {
                    put("id", "older-message")
                    put("type", "agent_message")
                    put("text", "Earlier reply")
                },
                subagent("item.started", "Earlier"),
            )
        val recent = listOf(subagent("item.started", "Review"))
        val keys = TimelineKeys()
        var entries by mutableStateOf(keys.stabilize("h", fixtureEntries(recent, chat = true)))
        val original = entries.single().key
        val list = LazyListState()
        compose.setContent {
            CairnTheme("light") {
                LazyColumn(state = list, modifier = Modifier.height(320.dp)) {
                    items(entries, key = { it.key }) { entry ->
                        if (entry.agents.isNotEmpty())
                            SubagentTeam(entry.agents, active = true, identity = entry.key)
                        else Text("Earlier reply", Modifier.height(56.dp))
                    }
                    items(20) { Text("Later reply $it", Modifier.height(56.dp)) }
                }
            }
        }
        compose.onNodeWithText("1 sous-agent").performClick()
        compose.onAllNodesWithTag("subagent").assertCountEquals(0)
        compose.runOnIdle { runBlocking { list.scrollToItem(0, 8) } }
        compose.runOnIdle {
            entries = keys.stabilize("h", fixtureEntries(older + recent, chat = true))
        }
        compose.onNodeWithText("2 sous-agents").assertIsDisplayed()
        compose.onAllNodesWithTag("subagent").assertCountEquals(0)
        compose.runOnIdle {
            assertEquals(1, list.firstVisibleItemIndex)
            assertEquals(8, list.firstVisibleItemScrollOffset)
            assertEquals(original, list.layoutInfo.visibleItemsInfo.first().key)
        }
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
