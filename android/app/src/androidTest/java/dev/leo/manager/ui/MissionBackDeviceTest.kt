package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.leo.manager.data.*
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MissionBackDeviceTest {
    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val main = Agent(MAIN_AGENT_ID, "Agent principal")
    private val daily =
        Task("daily", "Revue quotidienne", "Relire les changements du jour.", MAIN_AGENT_ID, cron = "0 9 * * *", nextRun = now + 3_600_000)
    private val done =
        Run("done-run", taskId = "daily", status = "succeeded", trigger = "schedule", startedAt = now - 7_200_000,
            finishedAt = now - 7_000_000, snapshot = Snapshot(task = daily, agent = main))

    private fun shell(command: String): String =
        InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(command).use {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(it).bufferedReader().readText()
        }

    private fun stream(state: LiveState): MockResponse {
        val frame = "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    /** The system back gesture from the left edge, slow enough to preview the screen below. */
    private fun backGesture() {
        val metrics = InstrumentationRegistry.getInstrumentation().targetContext.resources.displayMetrics
        val y = metrics.heightPixels / 2
        val swipe = thread { shell("input swipe 1 $y ${metrics.widthPixels * 2 / 3} $y 1500") }
        // Frames advance during the gesture, as outside tests, so the preview composes the screen below.
        compose.waitUntil(10000) { !swipe.isAlive }
    }

    @Test
    fun backGestureFromARunOpenedInTheMissionSheetReturnsToTheMission() {
        // The predictive preview follows the edge swipe of gesture navigation.
        shell("cmd overlay enable-exclusive --category com.android.internal.systemui.navbar.gestural")
        compose.waitUntil(10000) { shell("settings get secure navigation_mode").trim() == "2" }
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val json = { body: String -> MockResponse().setHeader("Content-Type", "application/json").setBody(body) }
                        return when {
                            path == "/api/session" -> json("{\"authenticated\":true,\"csrf\":\"fixture\"}")
                            path == "/api/agents" -> json(wireJson.encodeToString(listOf(main)))
                            path == "/api/tasks" -> json(wireJson.encodeToString(listOf(daily)))
                            path == "/api/tasks/activity" -> json(wireJson.encodeToString(listOf(done)))
                            path == "/api/runs" -> json(wireJson.encodeToString(listOf(done)))
                            path == "/api/schedule/preview" -> json("{\"occurrences\":[]}")
                            path == "/api/chats/stream" -> stream(LiveState(chats = emptyList()))
                            path == "/api/runs/done-run/stream" -> stream(LiveState(run = done))
                            path == "/api/overview" || path.endsWith("/models") || path == "/api/accounts" -> json("{}")
                            else -> json("[]")
                        }
                    }
                }
            val origin = server.url("/").toString()
            val app = ApplicationProvider.getApplicationContext<Application>()
            runBlocking { Preferences(app).setOrigin("") }
            val vm = LeoViewModel(app, MissionBackVault())
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                LeoTheme("light") { LeoApp(vm = vm) }
            }
            try {
                compose.waitUntil(15000) { compose.onAllNodesWithContentDescription("Missions").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithContentDescription("Missions").performClick()
                compose.waitUntil(15000) { compose.onAllNodesWithText("Revue quotidienne").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Revue quotidienne").performClick()
                compose.waitUntil(15000) { compose.onAllNodesWithText("Terminée").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Terminée").performScrollTo().performClick()
                compose.waitUntil(15000) {
                    compose.onAllNodesWithContentDescription("Options de l’exécution").fetchSemanticsNodes().isNotEmpty() &&
                        compose.onAllNodesWithTag("mission-sheet").fetchSemanticsNodes().isEmpty()
                }
                backGesture()
                // The run leaves and the mission it was opened from is shown again.
                compose.waitUntil(10000) {
                    compose.onAllNodesWithContentDescription("Options de l’exécution").fetchSemanticsNodes().isEmpty()
                }
                compose.waitUntil(10000) { compose.onAllNodesWithTag("mission-sheet").fetchSemanticsNodes().isNotEmpty() }
                // A second gesture closes the sheet and stays on Missions.
                backGesture()
                compose.waitUntil(10000) { compose.onAllNodesWithTag("mission-sheet").fetchSemanticsNodes().isEmpty() }
                assertEquals(1, compose.onAllNodesWithTag("missions").fetchSemanticsNodes().size)
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private class MissionBackVault : SessionVault {
    private val values = mutableMapOf<String, String>()
    override fun read(origin: String) = values[origin]
    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
