package build.cairn.app.ui

import android.app.Application
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/** A received update must display even while an earlier cache write is held. */
abstract class CacheRefreshCases {
    @get:Rule val compose = createComposeRule()
    @get:Rule val directory = TemporaryFolder()

    @Test
    fun networkUpdateDoesNotWaitForAnEarlierCacheWrite() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val firstWrite = AtomicBoolean(true)
        val cache =
            HistoryCache(
                directory.root,
                { _, bytes ->
                    if (firstWrite.compareAndSet(true, false)) {
                        entered.countDown()
                        check(release.await(20, TimeUnit.SECONDS))
                    }
                    bytes
                },
                { _, bytes -> bytes },
            )
        MockWebServer().use { server ->
            fun frame(id: Long): String {
                val batch =
                    LiveBatch(
                        listOf(
                            RunEvent(
                                id = id,
                                createdAt = id,
                                type = "chat.user",
                                text = "Output $id",
                                runId = "fixture",
                            )
                        ),
                        LiveState(chat = Chat(id = "f59b275f-667e-58c8-a788-33a78b29fa25")),
                        false,
                        false,
                        "v1:fixture:1",
                    )
                return "event: batch\nid: $id\ndata: ${wireJson.encodeToString(batch)}\n\n"
            }
            val first = frame(1)
            val second = frame(2)
            val streams = AtomicInteger()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val endpoint = request.requestUrl!!.encodedPath
                        if (
                            endpoint.endsWith("/chats/f59b275f-667e-58c8-a788-33a78b29fa25/stream")
                        ) {
                            if (streams.getAndIncrement() == 0)
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(first)
                            // A finite first stream reconnects independently of the UI collector.
                            // Never send batch 2 until batch 1's cache write is actually held.
                            check(entered.await(10, TimeUnit.SECONDS))
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(second + ": keepalive\n\n".repeat(1000))
                                .throttleBody(second.toByteArray().size.toLong(), 1, TimeUnit.DAYS)
                        }
                        val body =
                            when (endpoint) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/overview" -> "{}"
                                else ->
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
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
                }
            val vm = CairnViewModel(app, vault, beaconOrigin = "")
            // Exercise the production reader without introducing a product injection API.
            CairnViewModel::class.java.getDeclaredField("historyCache").apply {
                isAccessible = true
                set(vm, cache)
            }
            val models = ViewModelStore().apply { put("fixture", vm) }
            var mounted by mutableStateOf(true)
            var ready by mutableStateOf(false)
            Performance.store.clear()
            try {
                compose.setContent {
                    if (mounted) {
                        val workspace by vm.state.collectAsStateWithLifecycle()
                        LaunchedEffect(Unit) {
                            vm.state.first { it.ready }
                            vm.connect(server.url("/").toString())
                            ready = true
                        }
                        if (ready) {
                            val live =
                                rememberLive(
                                    vm,
                                    workspace,
                                    "/chats/f59b275f-667e-58c8-a788-33a78b29fa25/stream",
                                )
                            Text(live.events.lastOrNull()?.text ?: "Waiting")
                        }
                    }
                }
                compose.waitUntil(20000) { entered.count == 0L }
                compose.waitUntil(10000) {
                    Performance.store
                        .snapshot()
                        .operations
                        .filter { it.operation == PerfOperation.LIVE_REDUCE }
                        .sumOf { it.count } >= 2
                }
                // LIVE_REDUCE proves batch 2 is already parsed and applied upstream.
                val receivedAt = System.nanoTime()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Output 2").fetchSemanticsNodes().isNotEmpty()
                }
                val visibleAt = System.nanoTime()
                compose.onNodeWithText("Output 2").assertIsDisplayed()
                val holdUntil = visibleAt + 300_000_000
                compose.waitUntil(2000) { System.nanoTime() >= holdUntil }
                compose.onNodeWithText("Output 2").assertIsDisplayed()
                val releasedAt = System.nanoTime()
                release.countDown()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Output 2").fetchSemanticsNodes().isNotEmpty()
                }
                println(
                    "CACHE_REFRESH receivedWhileCacheWriteBlocked=true publicationDelayMs=${(visibleAt - receivedAt) / 1e6} cacheHeldAfterReceptionMs=${(releasedAt - receivedAt) / 1e6} " +
                        "visibleAfterReleaseMs=${(System.nanoTime() - releasedAt) / 1e6}"
                )
            } finally {
                release.countDown()
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
            }
        }
    }
}
