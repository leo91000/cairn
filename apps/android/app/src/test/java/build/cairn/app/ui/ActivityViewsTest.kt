package build.cairn.app.ui

import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.app.data.*
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ActivityViewsTest {
    @get:Rule val compose = createComposeRule()

    private fun item(id: Long, json: String) =
        RunEvent(
            id,
            1789375200000,
            "item.completed",
            "",
            mapOf("item" to wireJson.parseToJsonElement(json)),
        )

    @Test
    fun `chat problems expose details without expanding a session group`() {
        val events =
            listOf(
                RunEvent(1, 1000, "turn.started", ""),
                RunEvent(2, 2000, "error", "Permission denied"),
                RunEvent(3, 3000, "status", "interrupted"),
                RunEvent(4, 4000, "status", "succeeded"),
            )
        compose.setContent {
            ActivityFixture {
                Page {
                    timelineEntries(events, chat = true).forEach { entry ->
                        ChatNotice(presentActivity(entry.events.single()))
                    }
                }
            }
        }
        compose.onNodeWithText("Permission denied").assertIsDisplayed()
        compose.onNodeWithText("Exécution interrompue").assertIsDisplayed()
        compose.onNodeWithText("Travail commencé").assertDoesNotExist()
        compose.onNodeWithText("Exécution réussie").assertDoesNotExist()
        compose.onNodeWithText("Suivi de l’exécution").assertDoesNotExist()
        compose.onNodeWithText("Terminé").assertDoesNotExist()
    }

    @Test
    fun `session notices are readable and JSON is only shown on request`() {
        val events =
            listOf(
                RunEvent(1, 1000, "turn.completed", """{"type":"turn.completed"}"""),
                RunEvent(2, 1000, "status", "succeeded"),
                RunEvent(3, 1000, "status", "Using Codex account: Compte principal"),
            )
        compose.setContent { ActivityFixture { Page { events.forEach { ActivityCard(it) } } } }
        compose.onNodeWithText("Compte Codex sélectionné").assertExists()
        compose.onNodeWithText("Travail terminé").performClick()
        compose.onAllNodesWithText("\"type\"", substring = true).assertCountEquals(0)
        shot("activity-session")
        compose.onNodeWithText("Détails techniques · JSON / source").performClick()
        compose.onNodeWithText("\"type\"", substring = true).assertExists()
    }

    @Test
    fun `tools have native diff plan checks search and read presentations`() {
        val samples =
            listOf(
                "activity-diff" to
                    item(
                        10,
                        """{"type":"file_change","status":"completed","changes":[{"path":"apps/android/ui/Chat.kt","kind":"update","diff":"@@ -12,3 +12,4 @@\n-Text(rawJson)\n+ActivityCard(event)\n+ArtifactGallery(files)\n Spacer(12.dp)"}]}""",
                    ),
                "activity-plan" to
                    item(
                        11,
                        """{"type":"todo_list","status":"completed","items":[{"text":"Analyser les vues de l’application web","completed":true},{"text":"Créer les présentations natives","completed":true},{"text":"Vérifier les parcours Android","completed":false}]}""",
                    ),
                "activity-mcp" to
                    item(
                        12,
                        """{"type":"mcp_tool_call","server":"GitHub","tool":"get_workflow_checks","status":"completed","arguments":{"repository":"cairn/manager","pullRequest":42},"result":{"content":[{"type":"text","text":"{\"jobs\":[{\"name\":\"Tests Android\",\"conclusion\":\"success\"},{\"name\":\"Analyse du code\",\"conclusion\":\"success\"}]}"}]}}""",
                    ),
                "activity-read" to
                    item(
                        13,
                        """{"type":"command_execution","command":"cat README.md","status":"completed","exit_code":0,"aggregated_output":"# Cairn pour Android\n\nUne application **native** pour suivre vos agents.\n\n- Conversations en direct\n- Artifacts regroupés\n- Notifications sans Firebase"}""",
                    ),
                "activity-search" to
                    item(
                        14,
                        """{"type":"command_execution","command":"rg -n ActivityCard apps/android/","status":"completed","exit_code":0,"aggregated_output":"ui/LiveUi.kt:250:ActivityCard(event)\nui/ActivityViews.kt:42:fun ActivityCard(event: RunEvent)"}""",
                    ),
            )
        var selected by mutableStateOf(samples[0].second)
        compose.setContent {
            ActivityFixture { Page { key(selected.id) { ActivityCard(selected) } } }
        }
        samples.forEach { (name, event) ->
            compose.runOnIdle { selected = event }
            compose.onNodeWithText(presentActivity(event).title).performClick()
            compose.onAllNodesWithText("\"type\"", substring = true).assertCountEquals(0)
            when (name) {
                "activity-diff" ->
                    compose.onNodeWithText("+2 ajouts · −1 suppressions").assertExists()
                "activity-plan" -> compose.onNodeWithText("2 / 3 étapes terminées").assertExists()
                "activity-mcp" -> {
                    compose.onNodeWithText("2 / 2 vérifications réussies").assertExists()
                    compose.onNodeWithText("Tests Android").assertExists()
                }
                "activity-search" ->
                    compose.onNodeWithText("ui/LiveUi.kt · ligne 250").assertExists()
            }
            shot(name)
        }
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

@Composable
private fun ActivityFixture(content: @Composable () -> Unit) {
    CairnTheme("dark") {
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) { content() }
    }
}
