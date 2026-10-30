package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Scroll up through a long, tool-heavy chat on a device: pages and reconnects happen while the
 * reader is idle and must not move the visible text, until the first question is reached.
 */
@RunWith(AndroidJUnit4::class)
class HistoryScrollDeviceTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun toolGroupsCountCompletedCallsRatherThanHiddenEvents() {
        val events =
            (0L..64L).flatMap { index ->
                val id = 1000L + index * 100L
                val item = buildJsonObject {
                    put("id", "tool-$index")
                    put("type", "command_execution")
                    put("command", "echo ok")
                    put("status", "completed")
                    put("exit_code", 0)
                }
                listOf(
                    RunEvent(
                        id = id,
                        createdAt = id,
                        type = "item.started",
                        text = "",
                        payload = kotlinx.serialization.json.JsonObject(mapOf("item" to item)),
                    ),
                    RunEvent(
                        id = id + 1,
                        createdAt = id + 1,
                        type = "diagnostic",
                        text = "heartbeat",
                    ),
                    RunEvent(
                        id = id + 2,
                        createdAt = id + 2,
                        type = "item.completed",
                        text = "",
                        payload = kotlinx.serialization.json.JsonObject(mapOf("item" to item)),
                    ),
                )
            }
        val entries = TimelineKeys().stabilize("grouping", timelineEntries(events, chat = true))
        compose.setContent {
            CairnTheme("dark") {
                Column {
                    for (entry in entries) {
                        AgentActions(
                            entry.key,
                            entry.events.map(::presentActivity),
                            hideRunning = false,
                        )
                    }
                }
            }
        }
        compose.onAllNodesWithTag("agent-actions").assertCountEquals(3)
        compose.onAllNodesWithText("A lancé 30 commandes").assertCountEquals(2)
        compose.onNodeWithText("A lancé 5 commandes").assertIsDisplayed()
    }

    private val turns = 60
    private val history = "v2:long:1"

    private fun cards(page: List<RunEvent>): List<TimelineCard> = page.mapNotNull { event ->
        val user = event.type == "chat.user"
        val reply = event.id % 10L == 0L
        if (!user && !reply) return@mapNotNull null
        val turn = (event.id + 9) / 10
        val content =
            if (user)
                build.cairn.contracts.TimelineContent.A(
                    attachments = emptyList(),
                    sentAt = event.createdAt,
                    text = event.text,
                )
            else
                build.cairn.contracts.TimelineContent.B(
                    itemId = "answer-$turn",
                    state = "done",
                    text =
                        "Réponse $turn. L’étape $turn est vérifiée : les commandes passent et rien d’autre n’a changé.",
                )
        TimelineCard(
            id = "message:${event.id}",
            runId = event.runId,
            position = event.id,
            revision = event.id,
            source = event.id,
            turn = turn,
            attempt = 0,
            createdAt = event.createdAt,
            updatedAt = event.createdAt,
            content = content,
        )
    }

    private fun uiEvents(page: List<RunEvent>): List<RunEvent> =
        page
            .filterNot { it.id % 10L == 0L }
            .map { if (it.type == "chat.user") it.copy(timeline = true) else it }

    // Each turn folds into three rows: the question, one « A lancé 8 commandes » group, the answer.
    private val events =
        (1..turns).flatMap { turn ->
            val base = (turn - 1) * 10L
            listOf(
                RunEvent(
                    id = base + 1,
                    createdAt = base + 1,
                    type = "chat.user",
                    text = "Question $turn : peux-tu vérifier l’étape $turn ?",
                    runId = "ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090",
                )
            ) +
                (1..8).map { step ->
                    RunEvent(
                        id = base + 1 + step,
                        createdAt = base + 1 + step,
                        type = "item.completed",
                        text = "",
                        payload =
                            kotlinx.serialization.json.JsonObject(
                                mapOf(
                                    "item" to
                                        buildJsonObject {
                                            put("id", "cmd-$turn-$step")
                                            put("type", "command_execution")
                                            put("command", "./gradlew check --step $step")
                                            put("status", "completed")
                                            put("aggregated_output", "ok")
                                            put("exit_code", 0)
                                        }
                                )
                            ),
                        runId = "ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090",
                    )
                } +
                RunEvent(
                    id = base + 10,
                    createdAt = base + 10,
                    type = "item.completed",
                    text = "",
                    payload =
                        kotlinx.serialization.json.JsonObject(
                            mapOf(
                                "item" to
                                    buildJsonObject {
                                        put("id", "answer-$turn")
                                        put("type", "agent_message")
                                        put(
                                            "text",
                                            "Réponse $turn. L’étape $turn est vérifiée : les commandes passent et rien d’autre n’a changé.",
                                        )
                                    }
                            )
                        ),
                    runId = "ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090",
                )
        }

    @Test
    fun scrollingUpThroughAToolHeavyChatNeverMovesTheReader() {
        MockWebServer().use { server ->
            val pages = CopyOnWriteArrayList<Long>()
            val window = events.takeLast(100)
            val batch =
                wireJson.encodeToString(
                    LiveBatch(
                        uiEvents(window),
                        LiveState(
                            chat =
                                Chat(
                                    id = "ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090",
                                    title = "Longue conversation",
                                    agentName = "Main agent",
                                )
                        ),
                        reset = true,
                        more = false,
                        history = history,
                        oldest = window.first().id,
                        hasOlder = true,
                        entries = cards(window).map { build.cairn.contracts.TimelineChange.A(it) },
                    )
                )
            val frame = "event: batch\nid: ${events.last().id}\ndata: $batch\n\n"
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        if (path.endsWith("/history")) {
                            val before = request.requestUrl!!.queryParameter("before")!!.toLong()
                            pages.add(before)
                            val page = events.filter { it.id < before }.takeLast(100)
                            // A realistic round trip: the page lands after the fling has settled.
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBodyDelay(700, TimeUnit.MILLISECONDS)
                                .setBody(
                                    wireJson.encodeToString(
                                        HistoryPage(
                                            events = uiEvents(page),
                                            entries = cards(page),
                                            history = history,
                                            oldest = page.first().id,
                                            hasOlder = page.first().id > 1,
                                        )
                                    )
                                )
                        }
                        // A reconnect resumes after the cursor: nothing new to send.
                        if (
                            path ==
                                "/api/installations/fixture/api/chats/ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090/stream"
                        )
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(
                                    (if (
                                        request.requestUrl!!.queryParameter("after") in
                                            listOf(null, "0")
                                    )
                                        frame
                                    else "") + ": keepalive\n\n".repeat(10000)
                                )
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    TimeUnit.SECONDS,
                                )
                        if (path == "/api/installations/fixture/api/chats/stream")
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(": keepalive\n\n".repeat(10000))
                                .throttleBody(13, 1, TimeUnit.SECONDS)
                        val body =
                            when (path) {
                                "/api/installations/fixture/api/tasks" -> """{"tasks":[]}"""
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/projects" -> """{"projects":[]}"""
                                "/api/installations/fixture/api/agents" -> """{"agents":[]}"""
                                "/api/installations/fixture/api/skills" -> "[]"
                                "/api/installations/fixture/api/mcps" -> """{"mcps":[]}"""
                                else -> "{}"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val application = ApplicationProvider.getApplicationContext<Application>()

            var visible by mutableStateOf(true)
            val vm = CairnViewModel(application, beaconOrigin = "")
            runBlocking {
                vm.state.first { it.ready }
                vm.connect(server.url("/").toString())
            }
            try {
                compose.setContent {
                    val workspace by vm.state.collectAsStateWithLifecycle()
                    if (visible && workspace.session.authenticated)
                        CairnTheme("dark") {
                            Surface(Modifier.fillMaxSize()) {
                                ChatScreen(
                                    vm,
                                    workspace,
                                    "ec1fdad0-11d4-5c4c-9cbe-0e5f6fed7090",
                                    openChat = {},
                                    openRun = {},
                                )
                            }
                        }
                }
                compose.waitUntil(30000) {
                    compose
                        .onAllNodesWithText("Question $turns", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                Thread.sleep(1500)
                val list = compose.onNodeWithTag("conversation-history")
                // On screen: the stream above ends every few seconds, and its « Reconnexion… »
                // status must not move the conversation either.
                fun questions(): List<Pair<String, Float>> =
                    compose
                        .onAllNodes(hasText("Question ", substring = true))
                        .fetchSemanticsNodes()
                        .filter { it.boundsInRoot.height > 0 }
                        .map {
                            it.config[androidx.compose.ui.semantics.SemanticsProperties.Text]
                                .first()
                                .text
                                .substringBefore(" :") to it.boundsInRoot.top
                        }
                var swipes = 0
                while (
                    compose
                        .onAllNodesWithText("Question 1 :", substring = true)
                        .fetchSemanticsNodes()
                        .isEmpty()
                ) {
                    assertTrue(
                        "The first question must be reachable by scrolling up",
                        ++swipes < 80,
                    )
                    list.performTouchInput {
                        swipeDown(
                            startY = height * 0.3f,
                            endY = height * 0.85f,
                            durationMillis = 220,
                        )
                    }
                    compose.waitForIdle()
                    val settled = questions()
                    // Pages requested during the gesture land now, while the reader is idle.
                    Thread.sleep(1200)
                    compose.waitForIdle()
                    val after = questions().toMap()
                    for ((question, top) in settled) after[question]?.let {
                        assertEquals(
                            "$question moved while older history was loading",
                            top,
                            it,
                            0.5f,
                        )
                    }
                }
                compose.waitUntil(10000) { pages.size == 5 }
                assertEquals("Each older page is requested once", pages.distinct(), pages)
                Thread.sleep(1500)
                compose.runOnIdle { visible = false }
                compose.waitForIdle()
                compose.runOnIdle { visible = true }
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Question 1 :", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                Thread.sleep(1500)
                assertEquals("Reopening cached pages must not download them again", 5, pages.size)
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
