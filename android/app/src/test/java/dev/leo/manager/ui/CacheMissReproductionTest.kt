package dev.leo.manager.ui

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/** Regression of the 0.5 cache miss: reopening must not depend on a server response. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class CacheMissReproductionTest {
    @get:Rule val compose = createComposeRule()

    @Test fun `small conversation reopens before server responds`() = reopen(large = false)

    @Test
    fun `large tool output keeps recent messages available without network`() = reopen(large = true)

    @Test
    fun `older pages prepend without moving the visible message`() =
        reopen(large = false, paged = true)

    private fun reopen(large: Boolean, paged: Boolean = false) {
        val app = ApplicationProvider.getApplicationContext<Application>()
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
        try {
            MockWebServer().use { server ->
                val requests = CopyOnWriteArrayList<String>()
                val olderRequested = CountDownLatch(1)
                val olderRequests = CopyOnWriteArrayList<String>()
                val releaseOlder = CountDownLatch(1)
                val output = "x".repeat(if (large) 2200 * 1024 else 100)
                fun message(n: Int) =
                    RunEvent(
                        n.toLong(),
                        n.toLong(),
                        "chat.user",
                        if (n == 40) "Message témoin" else "Message %03d".format(n),
                    )
                val accepted = if (paged) 40 else 2
                val events =
                    if (paged) (21..40).map(::message)
                    else
                        listOf(
                            RunEvent(
                                1,
                                1,
                                "item.completed",
                                output,
                                mapOf(
                                    "item" to
                                        buildJsonObject {
                                            put("id", "tool")
                                            put("type", "command_execution")
                                            put("command", "fixture")
                                            put("status", "completed")
                                            put("aggregated_output", output)
                                            put("exit_code", 0)
                                        }
                                ),
                            ),
                            RunEvent(2, 2, "chat.user", "Message témoin"),
                        )
                val batch =
                    wireJson.encodeToString(
                        LiveBatch(
                            events,
                            LiveState(chat = Chat("diagnostic", title = "Diagnostic cache")),
                            reset = false,
                            more = false,
                            history = "v1:fixture:1",
                            oldest = if (paged) 21 else null,
                            hasOlder = paged,
                        )
                    )
                val frame = "event: batch\nid: $accepted\ndata: $batch\n\n"
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            if (request.requestUrl?.encodedPath == "/api/chats/stream") {
                                // Navigation subscribes to the list independently of this history
                                // stream.
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(": keepalive\n\n".repeat(10000))
                                    .throttleBody(13, 1, TimeUnit.SECONDS)
                            }
                            if (request.requestUrl?.encodedPath == "/api/chats/diagnostic/stream") {
                                requests.add(request.path!!)
                                // Hold the reconnect indefinitely: visible content must come from
                                // cache.
                                if (requests.size > 1)
                                    return MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE)
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(frame + ": waiting\n\n".repeat(1000))
                                    .throttleBody(
                                        frame.toByteArray().size.toLong(),
                                        1,
                                        TimeUnit.DAYS,
                                    )
                            }
                            if (request.path.orEmpty().contains("/history?") && !paged) {
                                // The trimmed cache still has older history: it is prefetched, not
                                // offered as a button.
                                olderRequests.add(
                                    request.requestUrl?.queryParameter("before").orEmpty()
                                )
                                return MockResponse()
                                    .setHeader("Content-Type", "application/json")
                                    .setBody(
                                        wireJson.encodeToString(
                                            HistoryPage(emptyList(), "v1:fixture:1", 1, false)
                                        )
                                    )
                            }
                            if (request.path.orEmpty().contains("/history?")) {
                                assertEquals("21", request.requestUrl?.queryParameter("before"))
                                assertEquals(
                                    "v1:fixture:1",
                                    request.requestUrl?.queryParameter("history"),
                                )
                                olderRequested.countDown()
                                check(releaseOlder.await(30, TimeUnit.SECONDS)) {
                                    "History response was not released"
                                }
                                return MockResponse()
                                    .setHeader("Content-Type", "application/json")
                                    .setBody(
                                        wireJson.encodeToString(
                                            HistoryPage(
                                                (1..20).map(::message),
                                                "v1:fixture:1",
                                                1,
                                                false,
                                            )
                                        )
                                    )
                            }
                            val body =
                                when (request.path?.substringBefore('?')) {
                                    "/api/session" ->
                                        "{\"authenticated\":true,\"csrf\":\"fixture\"}"
                                    "/api/agents",
                                    "/api/projects",
                                    "/api/tasks",
                                    "/api/skills",
                                    "/api/mcps" -> "[]"
                                    else -> "{}"
                                }
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(body)
                        }
                    }
                server.start()
                val vm = LeoViewModel(app, MemoryVault())
                var opened by mutableStateOf(true)
                compose.setContent {
                    val workspace by vm.state.collectAsStateWithLifecycle()
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(server.url("/").toString())
                    }
                    if (workspace.session.authenticated && opened) {
                        LeoTheme("dark") {
                            Surface(Modifier.fillMaxSize()) {
                                ChatScreen(vm, workspace, "diagnostic", openChat = {}, openRun = {})
                            }
                        }
                    }
                }
                try {
                    compose.waitUntil(20000) {
                        compose
                            .onAllNodesWithText("Message témoin")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Diagnostic cache").assertIsDisplayed()
                    compose.onNodeWithText("Message témoin").assertIsDisplayed()
                    compose.runOnIdle { opened = false }
                    compose.waitForIdle()
                    compose.waitUntil(5000) { vm.api.streamCalls.isEmpty() }
                    val key =
                        vm.historyCache.key(
                            vm.state.value.origin,
                            vm.api.csrf,
                            "/chats/diagnostic/stream",
                        )
                    val cached = runBlocking { vm.historyCache.read(key) }
                    assertNotNull("Both sizes retain recent messages", cached)
                    compose.runOnIdle { opened = true }
                    compose.waitForIdle()
                    compose.waitUntil(10000) {
                        // Drain the Android main looper while waiting for the reconnect.
                        compose
                            .onAllNodesWithText("Diagnostic cache")
                            .fetchSemanticsNodes()
                            .isNotEmpty() && requests.size >= 2
                    }
                    compose.waitForIdle()
                    assertEquals(
                        "/api/chats/diagnostic/stream?after=$accepted&history=v1%3Afixture%3A1&window=1",
                        requests[1],
                    )
                    compose.onNodeWithText("Diagnostic cache").assertIsDisplayed()
                    compose.onNodeWithText("Message témoin").assertIsDisplayed()
                    if (large) compose.waitUntil(10000) { "2" in olderRequests }
                    if (paged) {
                        // A short history is prefetched as soon as it is shown. Hold the
                        // response so the real reading position can be measured.
                        compose
                            .onNode(hasScrollAction() and hasTestTag("conversation-history"))
                            .performScrollToNode(hasText("Message 021"))
                        compose.waitUntil(10000) { olderRequested.count == 0L }
                        compose.waitForIdle()
                        val before =
                            compose
                                .onNodeWithText("Message 021")
                                .fetchSemanticsNode()
                                .boundsInRoot
                                .top
                        releaseOlder.countDown()
                        compose.waitUntil(10000) {
                            compose
                                .onNodeWithTag("conversation-history")
                                .fetchSemanticsNode()
                                .config[SemanticsProperties.CollectionInfo]
                                .rowCount == 40
                        }
                        // The page is prepended above the row Compose anchors on; asynchronous
                        // Markdown may need one more layout before the bounds are final.
                        compose.waitUntil(10000) {
                            val bounds =
                                compose
                                    .onAllNodesWithText("Message 021")
                                    .fetchSemanticsNodes()
                                    .firstOrNull()
                                    ?.boundsInRoot
                            bounds != null &&
                                bounds.height > 0 &&
                                kotlin.math.abs(bounds.top - before) <= 1f
                        }
                        compose.onNodeWithText("Message 021").assertIsDisplayed()
                        assertEquals(
                            before,
                            compose
                                .onNodeWithText("Message 021")
                                .fetchSemanticsNode()
                                .boundsInRoot
                                .top,
                            1f,
                        )
                        compose
                            .onNode(hasScrollAction() and hasTestTag("conversation-history"))
                            .performScrollToNode(hasText("Message 001"))
                        compose.onNodeWithText("Message 001").assertIsDisplayed()
                    }
                } finally {
                    releaseOlder.countDown()
                    compose.runOnIdle { opened = false }
                    compose.waitForIdle()
                    vm.api.closeStreams()
                }
            }
        } finally {
            androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }
}
