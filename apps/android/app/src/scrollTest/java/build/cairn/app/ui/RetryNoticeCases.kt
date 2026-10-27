package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
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
            id = "retry",
            status = "queued",
            accountWaitReason = "Temporary agent error. Retrying in 30 seconds (attempt 1/3).",
            retry =
                RunRetry(
                    attempt = 1,
                    limit = 5,
                    nextAttemptAt = deadline,
                    cause =
                        RunRetryCause(
                            kind = "timeout",
                            message = "Temporary timeout",
                        ),
                ),
            taskId = "",
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
        compose.onAllNodesWithTag("recovery-notice").assertCountEquals(2)
        compose.onAllNodesWithText("Reprise 1/5").assertCountEquals(2)
        capture("countdown")
        compose.waitUntil(6000) {
            compose.onAllNodesWithText("Bientôt").fetchSemanticsNodes().size == 2
        }
        capture("now")
    }

    @Test
    fun noticesFollowRunUpdatesAndKeepOtherWaitReasons() {
        val current = mutableStateOf(run(System.currentTimeMillis() + 60_000))
        compose.setContent { Notices(current.value) }
        compose.onAllNodesWithText("Reprise 1/5", substring = true).assertCountEquals(2)
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
        compose.onAllNodesWithText("Reprise 2/5", substring = true).assertCountEquals(2)
        capture("attempt")
        compose.runOnIdle {
            current.value = current.value.copy(accountWaitReason = "Waiting for capacity.")
        }
        compose.onAllNodesWithText("Waiting for capacity.", substring = true).assertCountEquals(2)
        compose.onAllNodesWithTag("recovery-notice").assertCountEquals(0)
        capture("priority")

        compose.runOnIdle {
            current.value = current.value.copy(accountWaitReason = null, status = "running")
        }
        compose.onAllNodesWithTag("recovery-notice").assertCountEquals(0)
        compose.onAllNodesWithText("Conversation en attente").assertCountEquals(0)
    }

    @Test
    fun retryCanBeCancelledAndBusyDisablesTheActionOnANarrowScreen() {
        var cancelled = 0
        val busy = mutableStateOf(false)
        compose.setContent {
            val current = remember { run(System.currentTimeMillis() + 80_000) }
            CairnTheme {
                CompositionLocalProvider(
                    LocalDensity provides Density(LocalDensity.current.density, 1.5f)
                ) {
                    Surface(Modifier.width(320.dp).safeDrawingPadding()) {
                        ChatRunWaitingNotice(
                            current,
                            {},
                            busy = busy.value,
                            cancelRetry = { cancelled++ },
                        )
                    }
                }
            }
        }
        val titleLayouts = mutableListOf<TextLayoutResult>()
        compose.onNodeWithText("Nouvelle tentative automatique").performSemanticsAction(
            SemanticsActions.GetTextLayoutResult
        ) {
            it(titleLayouts)
        }
        assertTrue(
            "Large text must remain readable on a narrow screen",
            titleLayouts.single().lineCount <= 3,
        )
        compose.onNodeWithText("Annuler la reprise").assertIsDisplayed().performClick()
        assertEquals(1, cancelled)
        compose.onNodeWithText("Voir le détail").performClick()
        compose.onNodeWithText("Temporary timeout").assertIsDisplayed()
        capture("narrow-details")
        compose.runOnIdle { busy.value = true }
        compose.onNodeWithText("Annuler la reprise").assertIsNotEnabled()
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
