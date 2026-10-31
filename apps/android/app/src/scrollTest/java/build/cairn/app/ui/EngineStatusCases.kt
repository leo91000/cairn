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

    private val retry =
        build.cairn.contracts.EngineRetry(
            attempt = 1,
            delayMs = 4000,
            httpStatus = 529,
            maxRetries = 10,
            noResponse = false,
            reason = "overloaded",
        )

    private fun engineRetry(
        position: Long,
        attempt: Long,
        prominent: Boolean,
        recovered: Boolean = false,
    ) =
        noticeCard(
            position,
            TimelineNotice(
                code = "engineRetry",
                engineRetry = retry.copy(attempt = attempt),
                prominent = prominent,
                recovered = recovered,
                state = "info",
            ),
        )

    private val limitCard =
        noticeCard(
            2,
            TimelineNotice(
                code = "usageLimit",
                prominent = true,
                state = "info",
                usageLimit =
                    build.cairn.contracts.UsageLimit(
                        blocked = false,
                        resetsAt = System.currentTimeMillis() + 3_600_000,
                        status = "allowed_warning",
                        window = "five_hour",
                    ),
            ),
        )

    private val warningCard =
        noticeCard(
            3,
            TimelineNotice(
                code = "engineNotice",
                detail = "Context is almost full.",
                level = "warning",
                prominent = true,
                state = "info",
            ),
        )

    @Test
    fun conversationShowsTheLatestRetryUntilWorkContinues() {
        // The server keeps only the latest engine retry prominent, until work continues.
        var cards by
            mutableStateOf(
                listOf(engineRetry(1, 1, false), limitCard, warningCard, engineRetry(4, 2, true))
            )
        compose.setContent { Screen { ConversationNotices(cards) } }
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 2/10")
            .assertIsDisplayed()
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 1/10")
            .assertDoesNotExist()
        compose.onNodeWithText("Limite de 5 heures bientôt atteinte").assertIsDisplayed()
        compose.onNodeWithText("Avertissement de Claude Code").assertIsDisplayed()
        capture("retrying")

        cards =
            listOf(
                engineRetry(1, 1, false),
                limitCard,
                warningCard,
                engineRetry(4, 2, false, recovered = true),
            )
        compose
            .onNodeWithText("Claude Code relance la requête", substring = true)
            .assertDoesNotExist()
        compose.onNodeWithText("Limite de 5 heures bientôt atteinte").assertIsDisplayed()
        compose.onNodeWithText("Avertissement de Claude Code").assertIsDisplayed()
        capture("continued")
    }

    @Test
    fun runLogMarksTheRetryWorkRecoveredFrom() {
        val cards = listOf(engineRetry(1, 1, false), engineRetry(2, 2, false, recovered = true))
        compose.setContent { Screen { cards.forEach { RunLogRow(it) } } }
        compose
            .onNodeWithText("Claude Code relance la requête · tentative 2/10")
            .assertIsDisplayed()
        compose.onAllNodesWithText(RECOVERED_STATE).assertCountEquals(1)
        capture("run-log")
    }
}
