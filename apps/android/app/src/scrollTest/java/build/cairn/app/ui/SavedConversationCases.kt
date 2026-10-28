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
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Rule
import org.junit.Test

/** Retained pager pages must recheck the Fil before showing their saved output on selection. */
abstract class SavedConversationCases {
    @get:Rule val compose = createComposeRule()

    @Test fun selectingAnOutdatedCachedPreviewWaitsForItsStream() = select(fromCache = true)

    @Test fun selectingAnOutdatedNetworkPreviewWaitsForItsStream() = select()

    @Test fun reselectingAnOutdatedRetainedPageWaitsForItsStream() = select(wasSelected = true)

    private fun select(fromCache: Boolean = false, wasSelected: Boolean = false) {
        MockWebServer().use { server ->
            val run =
                Run(
                    id = "run",
                    status = "succeeded",
                    finishedAt = 20,
                    taskId = "",
                )
            val chat = Chat(id = "saved", runId = run.id, updatedAt = 10, status = run.status)
            val oldState = LiveState(chat = chat, run = run)
            val event =
                RunEvent(
                    id = 1,
                    createdAt = 1,
                    type = "chat.user",
                    text = "Saved output",
                    runId = run.id,
                )
            val history = "v1:run:1"
            val path = "/chats/saved/stream"
            val requests = AtomicInteger()
            val requested = CountDownLatch(1)
            val release = CountDownLatch(1)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val endpoint = request.requestUrl!!.encodedPath
                        if (endpoint.endsWith(path)) {
                            val fresh = fromCache || requests.getAndIncrement() > 0
                            if (fresh) {
                                requested.countDown()
                                check(release.await(30, TimeUnit.SECONDS))
                            }
                            val batch =
                                LiveBatch(
                                    listOf(
                                        if (fresh) event.copy(id = 2, text = "Current output")
                                        else event
                                    ),
                                    if (fresh)
                                        LiveState(
                                            chat = chat.copy(updatedAt = 11),
                                            run = run.copy(finishedAt = 30),
                                        )
                                    else oldState,
                                    false,
                                    false,
                                    history,
                                )
                            val frame =
                                "event: batch\nid: ${if (fresh) 2 else 1}\ndata: ${wireJson.encodeToString(batch)}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": waiting\n\n".repeat(1000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    TimeUnit.DAYS,
                                )
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
                    override fun read(origin: String) = "cairn_session=fixture"

                    override fun write(origin: String, cookie: String?) = Unit
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var mounted by mutableStateOf(true)
            var prepared by mutableStateOf(false)
            var selected by mutableStateOf(wasSelected)
            var known by mutableStateOf(chat.copy(finishedAt = 20))
            try {
                compose.setContent {
                    if (mounted) {
                        val workspace by vm.state.collectAsStateWithLifecycle()
                        LaunchedEffect(Unit) {
                            vm.state.first { it.ready }
                            vm.connect(server.url("/").toString())
                            if (fromCache) {
                                val api = vm.api
                                vm.historyCache.save(
                                    vm.historyCache.key(api.cacheScope, api.csrf, path),
                                    CachedHistory(1, history, oldState, listOf(event)),
                                )
                            }
                            prepared = true
                        }
                        if (prepared) {
                            val live =
                                rememberLive(
                                    vm,
                                    workspace,
                                    path,
                                    streaming = selected,
                                    current = { savedIsCurrent(it, listOf(known)) },
                                )
                            Text(live.events.lastOrNull()?.text ?: "Waiting for current output")
                        }
                    }
                }
                compose.waitUntil(20000) {
                    compose.onAllNodesWithText("Saved output").fetchSemanticsNodes().isNotEmpty()
                }
                if (wasSelected) compose.runOnIdle { selected = false }
                compose.waitForIdle()
                compose.waitUntil(10000) { vm.api.streamCalls.isEmpty() }
                compose.runOnIdle {
                    known = known.copy(finishedAt = 30, updatedAt = 11)
                    selected = true
                }
                compose.waitForIdle()
                compose.waitUntil(10000) { requested.count == 0L }
                compose.onNodeWithText("Saved output").assertDoesNotExist()
                compose.onNodeWithText("Waiting for current output").assertIsDisplayed()

                release.countDown()
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Current output").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Current output").assertIsDisplayed()
            } finally {
                release.countDown()
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
            }
        }
    }
}
