package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import build.cairn.app.data.ChatWakeup
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
class WaitGroupViewsTest {
    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val failedRun = "7f3a91c2-2222-4333-8444-555555555555"
    private val nextRun = "c41e08d5-2222-4333-8444-555555555555"

    private fun run(id: String) = buildJsonObject {
        put("type", "run_finished")
        put("runId", id)
    }

    private val checks = buildJsonObject {
        put("type", "github_pr_checks")
        put("pluginId", "github")
        put("projectId", "project")
        put("prNumber", 377)
        put("headSha", "d5ca07fc")
    }

    private fun finished(id: String, status: String) = buildJsonObject {
        put("state", "completed")
        put("type", "run_finished")
        put("runId", id)
        put("status", status)
    }

    private fun group(mode: String, vararg conditions: JsonObject) = buildJsonObject {
        put("type", "group")
        put("mode", mode)
        put("conditions", JsonArray(conditions.toList()))
    }

    private fun wakeup(condition: JsonObject, vararg members: JsonObject) =
        ChatWakeup(
            "wait-${condition.hashCode()}",
            "message",
            "Fusionner une fois la doc construite, la migration testée et la CI passée.",
            1800,
            now - 7 * 60_000,
            now + 23 * 60_000,
            condition = condition,
            members = JsonArray(members.toList()),
        )

    private val pending =
        wakeup(
            group("all", run(failedRun), checks, run(nextRun)),
            buildJsonObject { put("observation", finished(failedRun, "failed")) },
            buildJsonObject {
                putJsonObject("display") {
                    put("title", "Waiting for required checks on PR #377")
                    put("short", "PR #377 checks")
                    putJsonArray("details") { add("d5ca07f") }
                }
                put("nextCheckAt", now + 18_000)
            },
            JsonObject(emptyMap()),
        )

    private val anyResult = buildJsonObject {
        put("state", "completed")
        put("type", "group")
        put("mode", "any")
        putJsonArray("members") {
            addJsonObject {
                put("condition", run(failedRun))
                put("result", JsonNull)
            }
            addJsonObject {
                put("condition", run(nextRun))
                put("result", finished(nextRun, "succeeded"))
            }
        }
    }

    @Composable
    private fun Fixture(theme: String, opened: MutableList<String>) {
        CairnTheme(theme) {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    CompositionLocalProvider(
                        LocalLiveWait provides
                            LiveWait(pending, paused = false, openRun = { opened += it })
                    ) {
                        WaitCard(ActivityWait(pending))
                    }
                    CompositionLocalProvider(LocalLiveWait provides LiveWait(null, false)) {
                        val any = wakeup(group("any", run(failedRun), run(nextRun)))
                        WaitCard(
                            ActivityWait(
                                any,
                                anyResult,
                                observed = true,
                                resumedAt = now - 60_000,
                            )
                        )
                    }
                }
            }
        }
    }

    @Test
    fun `a group lists each condition and folds once the conversation moved on`() {
        val opened = mutableListOf<String>()
        var theme by mutableStateOf("light")
        compose.setContent { Fixture(theme, opened) }

        compose.onNodeWithText("Attente de 3 résultats").assertIsDisplayed()
        compose.onNodeWithText("Tous").assertIsDisplayed()
        compose.onNodeWithText("1/3").assertIsDisplayed()
        compose.onNodeWithText("L’exécution a échoué").assertIsDisplayed()
        compose.onNodeWithText("PR #377 checks").assertIsDisplayed()
        compose.onNodeWithText("Attente de sa fin").assertIsDisplayed()
        compose.onAllNodesWithText("Contrôle dans", substring = true).assertCountEquals(1)
        shot("wait-group-light")

        // A run it waits for opens from its row.
        compose.onAllNodesWithTag("wait-member")[0].performClick()
        assertEquals(listOf(failedRun), opened)

        // The `any` wait resumed: its outcome names the first result and its list is folded.
        compose.onNodeWithText("Premier résultat · L’exécution est terminée").assertIsDisplayed()
        compose.onAllNodesWithTag("wait-member").assertCountEquals(3)
        compose.onNodeWithText("1/2").performClick()
        compose.onAllNodesWithTag("wait-member").assertCountEquals(5)
        compose.onNodeWithText("Inutile : un autre résultat est arrivé avant").assertIsDisplayed()

        theme = "dark"
        compose.waitForIdle()
        shot("wait-group-dark")
    }

    @Test
    fun `a delegated task row opens its conversation and displays its failure`() {
        val chatId = "a11ce123-2222-4333-8444-555555555555"
        val task = buildJsonObject {
            put("type", "task_finished")
            put("chatId", chatId)
        }
        val failed = buildJsonObject {
            put("state", "completed")
            put("type", "task_finished")
            put("status", "failed")
            put("error", "Review failed")
        }
        val waiting =
            wakeup(
                group("all", task, run(nextRun)),
                buildJsonObject { put("observation", failed) },
                JsonObject(emptyMap()),
            )
        val opened = mutableListOf<String>()
        compose.setContent {
            CairnTheme("light") {
                Surface {
                    CompositionLocalProvider(
                        LocalLiveWait provides LiveWait(waiting, false, openChat = { opened += it })
                    ) {
                        WaitCard(ActivityWait(waiting))
                    }
                }
            }
        }
        compose.onNodeWithText("Tâche déléguée").assertIsDisplayed()
        compose.onNodeWithText("a11ce123").assertIsDisplayed()
        compose.onNodeWithText("La tâche a échoué · Review failed").assertIsDisplayed()
        compose.onAllNodesWithTag("wait-member")[0].performClick()
        assertEquals(listOf(chatId), opened)
    }

    private fun shot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        File(dir, "$name.png").outputStream().use {
            compose
                .onRoot()
                .captureToImage()
                .asAndroidBitmap()
                .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }
}
