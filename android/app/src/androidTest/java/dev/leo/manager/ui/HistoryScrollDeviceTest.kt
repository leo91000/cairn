package dev.leo.manager.ui

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.leo.manager.data.*
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
    private val turns = 60
    private val history = "v1:long:1"

    // Each turn folds into three rows: the question, one « A lancé 8 commandes » group, the answer.
    private val events = (1..turns).flatMap { turn ->
        val base = (turn - 1) * 10L
        listOf(RunEvent(base + 1, base + 1, "chat.user", "Question $turn : peux-tu vérifier l’étape $turn ?")) +
            (1..8).map { step ->
                RunEvent(base + 1 + step, base + 1 + step, "item.completed", "", mapOf("item" to buildJsonObject {
                    put("id", "cmd-$turn-$step")
                    put("type", "command_execution")
                    put("command", "./gradlew check --step $step")
                    put("status", "completed")
                    put("aggregated_output", "ok")
                    put("exit_code", 0)
                }))
            } +
            RunEvent(base + 10, base + 10, "item.completed", "", mapOf("item" to buildJsonObject {
                put("id", "answer-$turn")
                put("type", "agent_message")
                put("text", "Réponse $turn. L’étape $turn est vérifiée : les commandes passent et rien d’autre n’a changé.")
            }))
    }

    @Test
    fun scrollingUpThroughAToolHeavyChatNeverMovesTheReader() {
        MockWebServer().use { server ->
            val pages = CopyOnWriteArrayList<Long>()
            val window = events.takeLast(100)
            val batch = wireJson.encodeToString(
                LiveBatch(window, LiveState(chat = Chat("long", "Longue conversation", agentName = "Main agent")),
                    reset = true, more = false, history = history, oldest = window.first().id, hasOlder = true)
            )
            val frame = "event: batch\nid: ${events.last().id}\ndata: $batch\n\n"
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.requestUrl!!.encodedPath
                    if (path.endsWith("/history")) {
                        val before = request.requestUrl!!.queryParameter("before")!!.toLong()
                        pages.add(before)
                        val page = events.filter { it.id < before }.takeLast(100)
                        // A realistic round trip: the page lands after the fling has settled.
                        return MockResponse().setHeader("Content-Type", "application/json")
                            .setBodyDelay(700, TimeUnit.MILLISECONDS)
                            .setBody(wireJson.encodeToString(HistoryPage(page, history, page.first().id, page.first().id > 1)))
                    }
                    // A reconnect resumes after the cursor: nothing new to send.
                    if (path == "/api/chats/long/stream")
                        return MockResponse().setHeader("Content-Type", "text/event-stream")
                            .setBody((if (request.requestUrl!!.queryParameter("after") in listOf(null, "0")) frame else "") + ": keepalive\n\n".repeat(10000))
                            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
                    if (path == "/api/chats/stream")
                        return MockResponse().setHeader("Content-Type", "text/event-stream")
                            .setBody(": keepalive\n\n".repeat(10000)).throttleBody(13, 1, TimeUnit.SECONDS)
                    val body = when (path) {
                        "/api/session" -> "{\"authenticated\":true,\"csrf\":\"fixture\"}"
                        "/api/agents", "/api/projects", "/api/tasks", "/api/skills", "/api/mcps" -> "[]"
                        else -> "{}"
                    }
                    return MockResponse().setHeader("Content-Type", "application/json").setBody(body)
                }
            }
            val application = ApplicationProvider.getApplicationContext<Application>()
            runBlocking { Preferences(application).setOrigin("") }
            val vm = LeoViewModel(application)
            runBlocking {
                vm.state.first { it.ready }
                vm.forget()
                vm.connect(server.url("/").toString())
            }
            try {
                compose.setContent {
                    val workspace by vm.state.collectAsStateWithLifecycle()
                    if (workspace.session.authenticated)
                        LeoTheme("dark") {
                            Surface(Modifier.fillMaxSize()) {
                                ChatScreen(vm, workspace, "long", openChat = {}, openRun = {})
                            }
                        }
                }
                compose.waitUntil(30000) { compose.onAllNodesWithText("Question $turns", substring = true).fetchSemanticsNodes().isNotEmpty() }
                Thread.sleep(1500)
                val list = compose.onNodeWithTag("conversation-history")
                // On screen: the stream above ends every few seconds, and its « Reconnexion… »
                // status must not move the conversation either.
                fun questions(): List<Pair<String, Float>> =
                    compose.onAllNodes(hasText("Question ", substring = true))
                        .fetchSemanticsNodes()
                        .filter { it.boundsInRoot.height > 0 }
                        .map { it.config[androidx.compose.ui.semantics.SemanticsProperties.Text].first().text.substringBefore(" :") to it.boundsInRoot.top }
                var swipes = 0
                while (compose.onAllNodesWithText("Question 1 :", substring = true).fetchSemanticsNodes().isEmpty()) {
                    assertTrue("The first question must be reachable by scrolling up", ++swipes < 80)
                    list.performTouchInput { swipeDown(startY = height * 0.3f, endY = height * 0.85f, durationMillis = 220) }
                    compose.waitForIdle()
                    val settled = questions()
                    // Pages requested during the gesture land now, while the reader is idle.
                    Thread.sleep(1200)
                    compose.waitForIdle()
                    val after = questions().toMap()
                    for ((question, top) in settled) after[question]?.let {
                        assertEquals("$question moved while older history was loading", top, it, 0.5f)
                    }
                }
                compose.waitUntil(10000) { pages.size == 5 }
                assertEquals("Each older page is requested once", pages.distinct(), pages)
                Thread.sleep(1500)
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
