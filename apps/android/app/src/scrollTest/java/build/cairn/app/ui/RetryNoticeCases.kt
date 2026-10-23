package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** The actual conversation/run notices tick from one immutable run, with no server or stream. */
abstract class RetryNoticeCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    private fun run(deadline: Long) =
        Run(
            "retry",
            status = "queued",
            accountWaitReason = "Temporary agent error. Retrying in 30 seconds (attempt 1/3).",
            retry = RunRetry(1, 3, deadline, RunRetryCause("timeout", "Temporary timeout")),
        )

    /** Like CairnApp, the background surface gives the run text its theme colours. */
    @Composable
    private fun Notices(run: Run) {
        CairnTheme {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                Column(Modifier.safeDrawingPadding()) {
                    ChatRunWaitingNotice(run, {})
                    RunWaitingReason(run)
                }
            }
        }
    }

    @Test
    fun conversationAndRunCountDownWithoutNewEvents() {
        compose.setContent {
            val run = remember { run(System.currentTimeMillis() + 4000) }
            Notices(run)
        }
        val notices = compose.onAllNodesWithText("Temporary agent error.", substring = true)
        notices.assertCountEquals(2)
        val initial =
            notices.fetchSemanticsNodes().map { it.config[SemanticsProperties.Text].toString() }
        assertTrue(initial.all { it.contains(Regex("Retrying in [1-4] seconds")) })
        capture("countdown")

        compose.waitUntil(5000) {
            notices.fetchSemanticsNodes().map { it.config[SemanticsProperties.Text].toString() } !=
                initial
        }
        compose.waitUntil(5000) {
            compose
                .onAllNodesWithText("Retrying now", substring = true)
                .fetchSemanticsNodes()
                .size == 2
        }
        capture("now")
    }

    @Test
    fun noticesFollowRunUpdatesAndKeepOtherWaitReasons() {
        val current = mutableStateOf(run(System.currentTimeMillis() + 60_000))
        compose.setContent { Notices(current.value) }
        compose.onAllNodesWithText("attempt 1/3", substring = true).assertCountEquals(2)
        compose.runOnIdle {
            current.value =
                current.value.copy(
                    retry =
                        current.value.retry!!.copy(
                            attempt = 2,
                            nextAttemptAt = System.currentTimeMillis() + 120_000,
                        )
                )
        }
        compose.onAllNodesWithText("attempt 2/3", substring = true).assertCountEquals(2)
        capture("attempt")
        compose.runOnIdle {
            current.value = current.value.copy(accountWaitReason = "Waiting for capacity.")
        }
        compose.onAllNodesWithText("Waiting for capacity.", substring = true).assertCountEquals(2)
        compose.onAllNodesWithText("Temporary agent error.", substring = true).assertCountEquals(0)
        capture("priority")

        compose.runOnIdle {
            current.value = current.value.copy(accountWaitReason = null, status = "running")
        }
        compose.onAllNodesWithText("Temporary agent error.", substring = true).assertCountEquals(0)
        compose.onAllNodesWithText("Conversation en attente").assertCountEquals(0)
    }

    @Test
    fun chatShowsOneRetryNoticePerFailureChainWithItsRetryTitle() {
        fun failure(id: Long, message: String) =
            RunEvent(
                id,
                id * 1000,
                "turn.failed",
                message,
                mapOf("error" to buildJsonObject { put("message", message) }),
            )
        fun retry(id: Long) =
            RunEvent(
                id,
                id * 1000,
                "retry.scheduled",
                "Temporary agent error.",
                mapOf(
                    "retry" to
                        buildJsonObject {
                            put("attempt", 1)
                            put("limit", 3)
                        }
                ),
            )
        val capacity = "Selected model is at capacity. Please try a different model."
        val connection = "WebSocket connection failed: 503 Service Unavailable"
        var events by
            mutableStateOf(
                listOf(RunEvent(1, 1000, "turn.started", ""), failure(2, capacity), retry(3))
            )
        compose.setContent {
            CairnTheme {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    Column(Modifier.safeDrawingPadding()) {
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
            }
        }
        compose
            .onNodeWithText("Erreur temporaire — nouvelle tentative automatique 1/3")
            .assertIsDisplayed()
        compose.onAllNodesWithText(capacity).assertCountEquals(1)
        capture("failure-retrying")

        events = events + RunEvent(4, 4000, "turn.started", "") + failure(5, capacity)
        compose
            .onNodeWithText("L’agent a rencontré une erreur après 1 nouvelle tentative automatique")
            .assertIsDisplayed()
        compose.onAllNodesWithText(capacity).assertCountEquals(1)

        // A retry title says more than the connection pattern of its reason.
        events = listOf(RunEvent(1, 1000, "turn.started", ""), failure(2, connection), retry(3))
        compose
            .onNodeWithText("Erreur temporaire — nouvelle tentative automatique 1/3")
            .assertIsDisplayed()
        compose.onNodeWithText("Connexion interrompue").assertDoesNotExist()
    }
}
