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
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** Diagnostic only: hold a real cache write while a second real SSE batch arrives. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class CacheRefreshDiagnosisTest {
    @get:Rule val compose = createComposeRule()
    @get:Rule val directory = TemporaryFolder()

    @Test
    fun networkUpdateWaitsForAnEarlierCacheWrite() {
        assumeTrue(System.getenv("CAIRN_MEASURE_ANDROID_CACHE") == "1")
        val app = ApplicationProvider.getApplicationContext<Application>()
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
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
                        listOf(RunEvent(id, id, "chat.user", "Output $id", runId = "fixture")),
                        LiveState(chat = Chat("cache-refresh")),
                        false,
                        false,
                        "v1:fixture:1",
                    )
                return "event: batch\nid: $id\ndata: ${wireJson.encodeToString(batch)}\n\n"
            }
            val first = frame(1)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val endpoint = request.requestUrl!!.encodedPath
                        if (endpoint.endsWith("/chats/cache-refresh/stream"))
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(first + frame(2) + ": keepalive\n\n".repeat(1000))
                                // Deliver batch 1 first so its write has started before batch 2.
                                .throttleBody(
                                    first.toByteArray().size.toLong(),
                                    250,
                                    TimeUnit.MILLISECONDS,
                                )
                        val body =
                            when (endpoint) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = CairnViewModel(app, MemoryVault(), beaconOrigin = "")
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
                            val live = rememberLive(vm, workspace, "/chats/cache-refresh/stream")
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
                val detached = System.getenv("CAIRN_CACHE_EXPECT_UNBLOCKED") == "1"
                if (detached) {
                    compose.waitUntil(10000) {
                        compose.onAllNodesWithText("Output 2").fetchSemanticsNodes().isNotEmpty()
                    }
                    compose.onNodeWithText("Output 2").assertIsDisplayed()
                } else {
                    compose.onNodeWithText("Output 1").assertIsDisplayed()
                    compose.onNodeWithText("Output 2").assertDoesNotExist()
                }
                val receivedAt = System.nanoTime()
                val holdUntil = receivedAt + 300_000_000
                compose.waitUntil(2000) { System.nanoTime() >= holdUntil }
                if (detached) compose.onNodeWithText("Output 2").assertIsDisplayed()
                else {
                    compose.onNodeWithText("Output 1").assertIsDisplayed()
                    compose.onNodeWithText("Output 2").assertDoesNotExist()
                }
                val releasedAt = System.nanoTime()
                release.countDown()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Output 2").fetchSemanticsNodes().isNotEmpty()
                }
                println(
                    "CACHE_REFRESH_DIAGNOSIS detached=$detached cacheHeldAfterReceptionMs=${(releasedAt - receivedAt) / 1e6} " +
                        "visibleAfterReleaseMs=${(System.nanoTime() - releasedAt) / 1e6}"
                )
            } finally {
                release.countDown()
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
                androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
            }
        }
    }
}
