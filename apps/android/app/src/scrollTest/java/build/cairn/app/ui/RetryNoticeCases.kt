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
}
