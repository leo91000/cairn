package build.cairn.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Rule
import org.junit.Test

/** Claude Code's retries, usage limit and warnings, as the conversation and run log show them. */
abstract class EngineStatusCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    private var serial = 0L

    private fun event(type: String, body: JsonObjectBuilder.() -> Unit = {}): RunEvent {
        val id = ++serial
        return RunEvent(
            id = id,
            createdAt = id * 1000,
            type = type,
            text = "",
            payload =
                kotlinx.serialization.json.JsonObject(
                    buildJsonObject {
                        put("type", type)
                        body()
                    }
                ),
            runId = "run",
        )
    }

    private fun retry(attempt: Int) =
        event("engine.retry") {
            put("attempt", attempt)
            put("maxRetries", 10)
            put("delayMs", 4000)
            put("status", 529)
            put("reason", "overloaded")
        }

    private fun step() =
        event("item.started") {
            put(
                "item",
                buildJsonObject {
                    put("id", "tool-$serial")
                    put("type", "command_execution")
                    put("command", "ls")
                    put("status", "in_progress")
                },
            )
        }

    private val limit =
        event("provider.limit") {
            put("status", "allowed_warning")
            put("window", "five_hour")
            put("blocked", false)
            put("resetsAt", System.currentTimeMillis() + 3_600_000)
        }

    private val warning =
        event("engine.notice") {
            put("level", "warning")
            put("message", "Context is almost full.")
        }

    @Composable
    private fun Screen(content: @Composable () -> Unit) {
        CairnTheme {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                Column(
                    Modifier.safeDrawingPadding(),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    content()
                }
            }
        }
    }

    @Test
    fun conversationShowsTheLatestRetryUntilWorkContinues() {
        val first = retry(1)
        val second = retry(2)
        var events by mutableStateOf(listOf(first, limit, warning, second))
        compose.setContent {
            Screen {
                timelineEntries(events, chat = true)
                    .filter { it.notice }
                    .forEach { entry ->
                        ChatNotice(
                            presentActivity(entry.events.single()),
                            entry.noticeTitle,
                            entry.retrying,
                        )
                    }
            }
        }
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 2/10")
            .assertIsDisplayed()
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 1/10")
            .assertDoesNotExist()
        compose.onNodeWithText("Limite de 5 heures bientôt atteinte").assertIsDisplayed()
        compose.onNodeWithText("Avertissement de Claude Code").assertIsDisplayed()
        capture("retrying")

        events = events + step()
        compose
            .onNodeWithText("Claude Code relance la requête", substring = true)
            .assertDoesNotExist()
        compose.onNodeWithText("Limite de 5 heures bientôt atteinte").assertIsDisplayed()
        compose.onNodeWithText("Avertissement de Claude Code").assertIsDisplayed()
        capture("continued")
    }

    @Test
    fun runLogMarksTheRetryWorkRecoveredFrom() {
        val events = listOf(retry(1), retry(2), step())
        compose.setContent {
            Screen {
                timelineEntries(events)
                    .flatMap { it.events }
                    .filter { it.type == "engine.retry" }
                    .forEach { ActivityCard(it) }
            }
        }
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 2/10")
            .assertIsDisplayed()
        compose.onAllNodesWithText(RECOVERED_STATE).assertCountEquals(1)
        capture("run-log")
    }
}
