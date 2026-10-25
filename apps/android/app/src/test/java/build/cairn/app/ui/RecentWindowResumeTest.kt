package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.*
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/** A reader at the latest output resumes on the recent window, with paging from its boundary. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class RecentWindowResumeTest {
    @get:Rule val compose = createComposeRule()

    private class Owner : LifecycleOwner {
        val registry = LifecycleRegistry(this)
        override val lifecycle: Lifecycle
            get() = registry
    }

    @Test
    fun `a recent window replaces the history and older pages load from its boundary`() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
        try {
            MockWebServer().use { server ->
                val revision = "v1:fixture:1"
                val streams = CopyOnWriteArrayList<String>()
                val olderRequests = CopyOnWriteArrayList<String>()
                val state = LiveState(chat = Chat("diagnostic", title = "Diagnostic"))
                fun message(n: Int) =
                    RunEvent(
                        n.toLong(),
                        n.toLong(),
                        "chat.user",
                        "Message %03d".format(n),
                        runId = "fixture",
                    )
                fun stream(cursor: Int, batch: LiveBatch): MockResponse {
                    val frame =
                        "event: batch\nid: $cursor\ndata: ${wireJson.encodeToString(batch)}\n\n"
                    // Deliver the batch, then hold the connection open.
                    return MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(frame + ": waiting\n\n".repeat(1000))
                        .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.DAYS)
                }
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            val url = request.requestUrl!!
                            if (url.encodedPath == "/api/installations/fixture/api/chats/stream")
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(": keepalive\n\n".repeat(10000))
                                    .throttleBody(13, 1, TimeUnit.SECONDS)
                            if (
                                url.encodedPath ==
                                    "/api/installations/fixture/api/chats/diagnostic/stream"
                            ) {
                                streams.add(request.path!!)
                                return when (streams.size) {
                                    1 ->
                                        stream(
                                            60,
                                            LiveBatch(
                                                (41..60).map(::message),
                                                state,
                                                false,
                                                false,
                                                revision,
                                                oldest = 41,
                                                hasOlder = true,
                                            ),
                                        )
                                    // The installation answers a long backlog with its recent
                                    // window.
                                    2 ->
                                        stream(
                                            200,
                                            LiveBatch(
                                                (181..200).map(::message),
                                                state,
                                                true,
                                                false,
                                                revision,
                                                oldest = 181,
                                                hasOlder = true,
                                            ),
                                        )
                                    else -> MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE)
                                }
                            }
                            if (url.encodedPath.endsWith("/chats/diagnostic/history")) {
                                val before = url.queryParameter("before").orEmpty()
                                olderRequests.add(before)
                                val end = before.toInt() - 1
                                return MockResponse()
                                    .setHeader("Content-Type", "application/json")
                                    .setBody(
                                        wireJson.encodeToString(
                                            HistoryPage(
                                                (end - 19..end).map(::message),
                                                revision,
                                                end - 19L,
                                                false,
                                            )
                                        )
                                    )
                            }
                            val body =
                                when (request.path?.substringBefore('?')) {
                                    "/api/installations" -> beaconInstallationsFixture()
                                    "/api/account/session" -> beaconAccountFixture("fixture")
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
                val vm = CairnViewModel(app, MemoryVault(), beaconOrigin = "")
                lateinit var owner: Owner
                var opened by mutableStateOf(true)
                compose.setContent {
                    val workspace by vm.state.collectAsStateWithLifecycle()
                    LaunchedEffect(Unit) {
                        vm.state.first { it.ready }
                        vm.connect(server.url("/").toString())
                    }
                    val testOwner = remember {
                        Owner().also { it.registry.currentState = Lifecycle.State.RESUMED }
                    }
                    owner = testOwner
                    if (workspace.session.authenticated && opened) {
                        CompositionLocalProvider(LocalLifecycleOwner provides testOwner) {
                            CairnTheme("dark") {
                                Surface(Modifier.fillMaxSize()) {
                                    ChatScreen(
                                        vm,
                                        workspace,
                                        "diagnostic",
                                        openChat = {},
                                        openRun = {},
                                    )
                                }
                            }
                        }
                    }
                }
                fun rows() =
                    compose
                        .onNodeWithTag("conversation-history")
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.CollectionInfo]
                        .rowCount
                try {
                    // The short history prefetches its older page as soon as it is shown.
                    compose.waitUntil(20000) {
                        compose.waitForIdle()
                        olderRequests.size == 1 &&
                            compose
                                .onAllNodesWithTag("conversation-history")
                                .fetchSemanticsNodes()
                                .isNotEmpty() &&
                            rows() == 40
                    }
                    compose.onNodeWithText("Message 060").assertIsDisplayed()

                    // Returning while following asks for the recent window.
                    compose.runOnIdle { owner.registry.currentState = Lifecycle.State.CREATED }
                    compose.waitUntil(5000) { vm.api.streamCalls.isEmpty() }
                    compose.runOnIdle { owner.registry.currentState = Lifecycle.State.RESUMED }
                    compose.waitUntil(10000) {
                        compose.waitForIdle()
                        olderRequests.size == 2
                    }
                    assertEquals(
                        "/api/installations/fixture/api/chats/diagnostic/stream?after=60&history=v1%3Afixture%3A1&window=1&recent=1",
                        streams[1],
                    )
                    // Pages loaded above the replaced events are dropped: paging resumes at
                    // the new boundary, never leaving a silent gap before message 181.
                    assertEquals(listOf("41", "181"), olderRequests.toList())
                    compose.waitUntil(10000) {
                        compose.waitForIdle()
                        rows() == 40
                    }
                    compose.onNodeWithText("Message 200").assertIsDisplayed()
                    compose
                        .onNode(hasScrollAction() and hasTestTag("conversation-history"))
                        .performScrollToNode(hasText("Message 161"))
                    compose.onNodeWithText("Message 161").assertIsDisplayed()
                    assertTrue(
                        compose.onAllNodesWithText("Message 040").fetchSemanticsNodes().isEmpty()
                    )
                } finally {
                    compose.runOnIdle {
                        owner.registry.currentState = Lifecycle.State.DESTROYED
                        opened = false
                    }
                    compose.waitForIdle()
                    vm.api.closeStreams()
                }
            }
        } finally {
            androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }
}
