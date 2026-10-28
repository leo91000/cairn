package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfScreen
import build.cairn.app.diagnostics.Performance
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** A mission opened from the Fil reads beside its conversations, and real swipes follow the Fil. */
@RunWith(AndroidJUnit4::class)
class FilPagerDeviceTest {
    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val main =
        Agent(
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            revision = "",
            createdAt = 0,
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    projects = null,
                    skills = null,
                    mcps = null,
                ),
            id = MAIN_AGENT_ID,
            name = "Agent principal",
        )
    private val daily =
        Task(
            "a05f5991-ec1e-57a2-a8e5-ec314581a110",
            "Revue quotidienne",
            "Relire les changements du jour.",
            MAIN_AGENT_ID,
            cron = "0 9 * * *",
            nextRun = now + 3_600_000,
        )
    private val failed =
        Run(
            id = "failed-run",
            taskId = "a05f5991-ec1e-57a2-a8e5-ec314581a110",
            status = "failed",
            trigger = "schedule",
            startedAt = now - 7_200_000,
            finishedAt = now - 7_000_000,
            snapshot = Snapshot(task = daily.runTaskSnapshot(), agent = main),
        )
    private val chats =
        listOf(
            Chat(
                id = "question",
                title = "Choisir la pagination",
                agentName = "Agent principal",
                pendingQuestions = 1,
                updatedAt = now - 60_000,
            ),
            Chat(
                id = "live",
                title = "Refonte de l’application",
                agentName = "Agent principal",
                status = "running",
                runId = "live-run",
                run =
                    Run(
                        id = "live-run",
                        status = "running",
                        startedAt = now - 14 * 60_000,
                        taskId = "",
                    ),
                updatedAt = now - 120_000,
            ),
        )

    private fun shell(command: String): String =
        InstrumentationRegistry.getInstrumentation().uiAutomation.executeShellCommand(command).use {
            android.os.ParcelFileDescriptor.AutoCloseInputStream(it).bufferedReader().readText()
        }

    private fun stream(state: LiveState): MockResponse {
        val frame =
            "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    /** A horizontal finger swipe across the middle of the screen, as a user makes it. */
    private fun swipe(left: Boolean) {
        val metrics =
            InstrumentationRegistry.getInstrumentation().targetContext.resources.displayMetrics
        val y = metrics.heightPixels / 2
        val (from, to) = (metrics.widthPixels * 85 / 100) to (metrics.widthPixels * 15 / 100)
        val gesture = thread {
            shell("input swipe ${if (left) from else to} $y ${if (left) to else from} $y 300")
        }
        compose.waitUntil(10000) { !gesture.isAlive }
    }

    private fun waitPage(tag: String) =
        compose.waitUntil(15000) {
            compose.onAllNodes(hasTestTag(tag) and isSelected()).fetchSemanticsNodes().isNotEmpty()
        }

    private fun waitText(value: String) =
        compose.waitUntil(15000) {
            compose.onAllNodesWithText(value).fetchSemanticsNodes().isNotEmpty()
        }

    @Test
    fun missionOpensBesideConversationsAndSwipesFollowTheFil() {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val json = { body: String ->
                            MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(body)
                        }
                        return when {
                            path == "/api/installations" -> json(beaconInstallationsFixture())
                            path == "/api/account/session" -> json(beaconAccountFixture("fixture"))
                            path == "/api/installations/fixture/api/agents" ->
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(main))
                                    )
                                )
                            path == "/api/installations/fixture/api/tasks" ->
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListMissionsOutput(listOf(daily))
                                    )
                                )
                            path == "/api/installations/fixture/api/tasks/activity" ->
                                json(wireJson.encodeToString(listOf(failed)))
                            path == "/api/installations/fixture/api/runs" ->
                                json(wireJson.encodeToString(listOf(failed)))
                            path == "/api/installations/fixture/api/schedule/preview" ->
                                json("{\"occurrences\":[]}")
                            path == "/api/installations/fixture/api/chats/stream" ->
                                stream(LiveState(chats = chats))
                            path == "/api/installations/fixture/api/runs/failed-run/stream" ->
                                stream(LiveState(run = failed))
                            path.startsWith("/api/installations/fixture/api/chats/") &&
                                path.endsWith("/stream") ->
                                stream(
                                    LiveState(
                                        chat =
                                            chats.first {
                                                path.endsWith("/chats/${it.id}/stream")
                                            }
                                    )
                                )
                            path == "/api/installations/fixture/api/overview" ||
                                path.endsWith("/models") ||
                                path == "/api/installations/fixture/api/accounts" -> json("{}")
                            else ->
                                json(
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/agents") ==
                                            true
                                    )
                                        """{"agents":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/mcps") ==
                                            true
                                    )
                                        """{"mcps":[]}"""
                                    else "[]"
                                )
                        }
                    }
                }
            val origin = server.url("/").toString()
            val app = ApplicationProvider.getApplicationContext<Application>()

            val vm = CairnViewModel(app, FilPagerVault(), beaconOrigin = "")
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme("light") { CairnApp(vm = vm) }
            }
            try {
                waitText("Revue quotidienne")
                compose.onNodeWithText("Revue quotidienne").performClick()
                waitPage("run-page:failed-run")
                compose.runOnIdle { assertEquals(PerfScreen.RUN, Performance.screen) }
                // The mission itself is one tab away from its run.
                waitText("Mission")
                compose.onNodeWithText("Mission").performClick()
                waitText("Lancer maintenant")
                // Pour vous: the question, then this mission; then the live conversation.
                swipe(left = true)
                waitPage("chat-page:live")
                compose.runOnIdle { assertEquals(PerfScreen.CHAT, Performance.screen) }
                swipe(left = false)
                waitPage("run-page:failed-run")
                compose.runOnIdle { assertEquals(PerfScreen.RUN, Performance.screen) }
                swipe(left = false)
                waitPage("chat-page:question")
                compose.runOnIdle { assertEquals(PerfScreen.CHAT, Performance.screen) }
                compose.onNodeWithContentDescription("Retour").performClick()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithTag("fil").fetchSemanticsNodes().isNotEmpty()
                }
                compose.runOnIdle { assertEquals(PerfScreen.FIL, Performance.screen) }
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private class FilPagerVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
