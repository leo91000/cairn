package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class OlderPagesScopeTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun `changing chat cancels an outstanding page and allows paging the new chat`() {
        switchScope(changeApi = false)
    }

    @Test
    fun `changing API cancels an outstanding page and allows paging the new scope`() {
        switchScope(changeApi = true)
    }

    private fun switchScope(changeApi: Boolean) {
        val app = ApplicationProvider.getApplicationContext<Application>()
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
        try {
            MockWebServer().use { first ->
                MockWebServer().use { second ->
                    fun fixture(server: MockWebServer, label: String) {
                        server.dispatcher =
                            object : Dispatcher() {
                                override fun dispatch(request: RecordedRequest): MockResponse {
                                    val path = request.requestUrl!!.encodedPath
                                    if (path.endsWith("/history")) {
                                        val response =
                                            MockResponse()
                                                .setHeader("Content-Type", "application/json")
                                                .setBody(
                                                    wireJson.encodeToString(
                                                        HistoryPage(
                                                            listOf(
                                                                RunEvent(
                                                                    9,
                                                                    9,
                                                                    "chat.user",
                                                                    "Older $label",
                                                                    runId = "r1",
                                                                )
                                                            ),
                                                            "v1:r1:1",
                                                            9,
                                                            false,
                                                        )
                                                    )
                                                )
                                        // Keep reads progressing until cancellation, rather than
                                        // letting OkHttp's read timeout finish the call first.
                                        if (label == "first" && path.endsWith("/one/history"))
                                            response.throttleBody(1, 1, TimeUnit.SECONDS)
                                        return response
                                    }
                                    if (
                                        path.endsWith("/one/stream") || path.endsWith("/two/stream")
                                    ) {
                                        val batch =
                                            LiveBatch(
                                                listOf(
                                                    RunEvent(
                                                        10,
                                                        10,
                                                        "chat.user",
                                                        "$label $path",
                                                        runId = "r1",
                                                    )
                                                ),
                                                LiveState(chat = Chat("fixture", title = "Test")),
                                                false,
                                                false,
                                                "v1:r1:1",
                                                oldest = 10,
                                                hasOlder = true,
                                            )
                                        val frame =
                                            "event: batch\nid: 10\ndata: ${wireJson.encodeToString(batch)}\n\n"
                                        return MockResponse()
                                            .setHeader("Content-Type", "text/event-stream")
                                            .setBody(frame + ": waiting\n\n".repeat(1000))
                                            .throttleBody(
                                                frame.toByteArray().size.toLong(),
                                                1,
                                                TimeUnit.DAYS,
                                            )
                                    }
                                    if (path.endsWith("/stream"))
                                        return MockResponse()
                                            .setSocketPolicy(SocketPolicy.NO_RESPONSE)

                                    val body =
                                        when (path) {
                                            "/api/account/session" -> beaconAccountFixture()
                                            "/api/installations" -> beaconInstallationsFixture()
                                            "/api/installations/fixture/api/projects" ->
                                                """{"projects":[]}"""
                                            "/api/installations/fixture/api/agents",
                                            "/api/installations/fixture/api/tasks",
                                            "/api/installations/fixture/api/skills",
                                            "/api/installations/fixture/api/mcps" -> "[]"
                                            else -> "{}"
                                        }
                                    return MockResponse()
                                        .setHeader("Content-Type", "application/json")
                                        .setBody(body)
                                }
                            }
                        server.start()
                    }
                    fixture(first, "first")
                    fixture(second, "second")
                    val vm = CairnViewModel(app, MemoryVault(), beaconOrigin = "")
                    var origin by mutableStateOf(first.url("/").toString())
                    var path by mutableStateOf("/chats/one/stream")
                    var opened by mutableStateOf(true)
                    var connectedOrigin by mutableStateOf<String?>(null)
                    var shown: LiveSnapshot? = null
                    compose.setContent {
                        val workspace by vm.state.collectAsStateWithLifecycle()
                        LaunchedEffect(origin) {
                            val target = origin
                            vm.state.first { it.ready }
                            vm.connect(target)
                            connectedOrigin = target
                        }
                        if (opened && workspace.session.authenticated) {
                            val live = rememberLive(vm, workspace, path)
                            SideEffect { shown = live }
                            androidx.compose.material3.Text(path)
                        }
                    }
                    try {
                        compose.waitUntil(20000) {
                            shown?.hasOlder == true && connectedOrigin == first.url("/").toString()
                        }
                        val previousApi = vm.api
                        compose.runOnIdle { checkNotNull(shown).loadOlder() }
                        compose.waitUntil(10000) {
                            previousApi.http.dispatcher.runningCalls().any {
                                it.request().url.encodedPath.endsWith("/history")
                            }
                        }
                        val pending =
                            previousApi.http.dispatcher.runningCalls().single {
                                it.request().url.encodedPath.endsWith("/history")
                            }
                        compose.runOnIdle {
                            assertTrue(
                                "The page must still be in flight when the scope changes",
                                previousApi.http.dispatcher.runningCalls().contains(pending),
                            )
                            if (changeApi) origin = second.url("/").toString()
                            else path = "/chats/two/stream"
                        }
                        compose.waitForIdle()
                        compose.waitUntil(5000) { pending.isCanceled() }
                        compose.waitUntil(20000) {
                            connectedOrigin == origin &&
                                shown
                                    ?.events
                                    ?.singleOrNull()
                                    ?.text
                                    ?.contains(if (changeApi) "second" else "/two/") == true
                        }
                        compose.runOnIdle {
                            assertFalse(checkNotNull(shown).loadingOlder)
                            assertNull(checkNotNull(shown).olderError)
                            checkNotNull(shown).loadOlder()
                        }
                        compose.waitUntil(10000) { shown?.events?.firstOrNull()?.id == 9L }
                        assertFalse(checkNotNull(shown).hasOlder)
                        assertEquals(9L, checkNotNull(shown).oldest)
                        val currentApi = vm.api
                        val cacheKey =
                            vm.historyCache.key(currentApi.cacheScope, currentApi.csrf, path)
                        compose.waitUntil(10000) {
                            runBlocking { vm.historyCache.read(cacheKey)?.oldest == 9L }
                        }
                        val cached = runBlocking { checkNotNull(vm.historyCache.read(cacheKey)) }
                        assertEquals(listOf(9L, 10L), cached.events.map { it.id })
                        assertFalse(cached.hasOlder)
                    } finally {
                        compose.runOnIdle { opened = false }
                        compose.waitForIdle()
                        vm.api.closeStreams()
                    }
                }
            }
        } finally {
            androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }
}
