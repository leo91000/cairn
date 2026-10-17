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
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import build.cairn.app.data.RunEvent
import java.io.File
import kotlinx.coroutines.runBlocking
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
class SubagentViewsTest {
    @get:Rule val compose = createComposeRule()

    private var serial = 0L

    private fun item(type: String, body: JsonObjectBuilder.() -> Unit): RunEvent {
        val id = ++serial
        return RunEvent(
            id,
            System.currentTimeMillis() - 60_000 + id * 1000,
            type,
            "",
            mapOf("item" to buildJsonObject(body)),
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

    private val team = timelineEntries(events, chat = true).single().agents

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
        var entries by mutableStateOf(keys.stabilize("h", timelineEntries(recent, chat = true)))
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
            entries = keys.stabilize("h", timelineEntries(older + recent, chat = true))
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
