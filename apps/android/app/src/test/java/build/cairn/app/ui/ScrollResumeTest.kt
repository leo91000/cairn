package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
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

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ScrollResumeTest {
    @get:Rule val compose = createComposeRule()

    private class Owner : LifecycleOwner {
        val registry = LifecycleRegistry(this)
        override val lifecycle: Lifecycle
            get() = registry
    }

    @Test
    fun `history opens atomically and foreground resumes cursor and reading position`() {
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
                val chat =
                    Chat(
                        "diagnostic",
                        title = "Diagnostic",
                        agentName = "Cairn",
                        status = "succeeded",
                    )
                fun block(end: Int, empty: Boolean = false): String {
                    val events =
                        if (empty) emptyList()
                        else
                            (end - 19..end).map { n ->
                                RunEvent(
                                    n.toLong(),
                                    n.toLong(),
                                    "chat.user",
                                    "Message %03d\nUne ligne de diagnostic.\nUne autre ligne de diagnostic."
                                        .format(n),
                                )
                            }
                    val json =
                        wireJson.encodeToString(
                            LiveBatch(
                                events,
                                LiveState(chat = chat),
                                false,
                                end < 60,
                                history = "v1:fixture:1",
                            )
                        )
                    val frame = "event: batch\nid: $end\ndata: $json\n\n"
                    val padding = 8192 - frame.toByteArray().size - 4
                    require(padding > 0)
                    return frame + ": " + "x".repeat(padding) + "\n\n"
                }
                val keepalive = (": " + "x".repeat(8188) + "\n\n").repeat(60)
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            if (
                                request.requestUrl?.encodedPath ==
                                    "/api/installations/fixture/api/chats/stream"
                            ) {
                                // Navigation subscribes to the list independently of this history
                                // stream.
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(": keepalive\n\n".repeat(10000))
                                    .throttleBody(13, 1, TimeUnit.SECONDS)
                            }
                            if (
                                request.requestUrl?.encodedPath ==
                                    "/api/installations/fixture/api/chats/diagnostic/stream"
                            ) {
                                requests.add(request.path!!)
                                val history =
                                    if (request.requestUrl?.queryParameter("after") == "60")
                                        block(60, true)
                                    else block(20) + block(40) + block(60)
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(history + keepalive)
                                    .throttleBody(8192, 2, TimeUnit.SECONDS)
                            }
                            val result =
                                when (request.path?.substringBefore('?')) {
                                    "/api/installations" -> beaconInstallationsFixture()
                                    "/api/account/session" -> beaconAccountFixture("fixture")
                                    "/api/installations/fixture/api/agents",
                                    "/api/installations/fixture/api/projects",
                                    "/api/installations/fixture/api/tasks",
                                    "/api/installations/fixture/api/skills",
                                    "/api/installations/fixture/api/mcps" -> "[]"
                                    else -> "{}"
                                }
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(result)
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
                fun visible(number: Int): Boolean =
                    compose
                        .onAllNodesWithText("Message %03d".format(number), substring = true)
                        .fetchSemanticsNodes()
                        .any { it.boundsInRoot.height > 0 }
                fun waitLast() {
                    compose.waitUntil(15000) { visible(60) }
                    compose.onNodeWithText("Message 060", substring = true).assertIsDisplayed()
                }
                compose.waitUntil(10000) {
                    compose.waitForIdle()
                    requests.size == 1
                }
                // Both partial pages must remain hidden, rather than scrolling through them.
                repeat(4) {
                    Thread.sleep(600)
                    compose.waitForIdle()
                    assertFalse(visible(20))
                    assertFalse(visible(40))
                }
                waitLast()
                compose.runOnIdle { owner.registry.currentState = Lifecycle.State.CREATED }
                compose.waitUntil(5000) { vm.api.streamCalls.isEmpty() }
                assertTrue(visible(60))
                compose.runOnIdle { owner.registry.currentState = Lifecycle.State.RESUMED }
                compose.waitUntil(5000) {
                    compose.waitForIdle()
                    requests.size == 2
                }
                Thread.sleep(300)
                compose.waitForIdle()
                assertTrue(visible(60))
                assertEquals(
                    "/api/installations/fixture/api/chats/diagnostic/stream?after=60&history=v1%3Afixture%3A1&window=1",
                    requests[1],
                )
                // A reader who scrolled upward must stay at the same offset on resume.
                val composerTop =
                    compose
                        .onNodeWithTag("conversation-composer")
                        .fetchSemanticsNode()
                        .boundsInRoot
                        .top
                compose
                    .onNode(hasScrollAction() and hasTestTag("conversation-history"))
                    .performTouchInput { swipeDown() }
                compose.waitForIdle()
                compose.onNodeWithContentDescription("Derniers messages").assertExists()
                val viewport =
                    compose.onNodeWithTag("conversation-history").fetchSemanticsNode().boundsInRoot
                val arrow =
                    compose
                        .onNodeWithContentDescription("Derniers messages")
                        .fetchSemanticsNode()
                        .boundsInRoot
                assertTrue(
                    "The latest-message control floats centred above the composer",
                    arrow.bottom <= viewport.bottom &&
                        arrow.top >= viewport.bottom - 72f &&
                        arrow.bottom <= composerTop &&
                        kotlin.math.abs(
                            (arrow.left + arrow.right) / 2 - (viewport.left + viewport.right) / 2
                        ) < 1f,
                )
                assertEquals(
                    "Showing the overlay does not move the composer",
                    composerTop,
                    compose
                        .onNodeWithTag("conversation-composer")
                        .fetchSemanticsNode()
                        .boundsInRoot
                        .top,
                    0.1f,
                )
                fun position() =
                    compose
                        .onNode(hasScrollAction() and hasTestTag("conversation-history"))
                        .fetchSemanticsNode()
                        .config[SemanticsProperties.VerticalScrollAxisRange]
                        .value()
                val before = position()
                compose.runOnIdle { owner.registry.currentState = Lifecycle.State.CREATED }
                compose.waitUntil(5000) { vm.api.streamCalls.isEmpty() }
                compose.runOnIdle { owner.registry.currentState = Lifecycle.State.RESUMED }
                compose.waitUntil(5000) {
                    compose.waitForIdle()
                    requests.size == 3
                }
                Thread.sleep(300)
                compose.waitForIdle()
                assertEquals(before, position(), 0.01f)
                assertEquals(
                    "/api/installations/fixture/api/chats/diagnostic/stream?after=60&history=v1%3Afixture%3A1&window=1",
                    requests[2],
                )
                compose.runOnIdle { opened = false }
                compose.waitForIdle()
                compose.waitUntil(5000) { vm.api.streamCalls.isEmpty() }
                compose.runOnIdle { opened = true }
                compose.waitForIdle()
                compose.waitUntil(5000) {
                    compose.waitForIdle()
                    requests.size == 4
                }
                Thread.sleep(300)
                compose.waitForIdle()
                assertEquals(before, position(), 0.01f)
                assertEquals(
                    "/api/installations/fixture/api/chats/diagnostic/stream?after=60&history=v1%3Afixture%3A1&window=1",
                    requests[3],
                )
                // Keep a real drag active long enough for the fade-out to finish.
                val history = compose.onNodeWithTag("conversation-history")
                history.performTouchInput {
                    down(center)
                    moveBy(Offset(0f, 80f), delayMillis = 200)
                }
                compose.mainClock.advanceTimeBy(250)
                compose.waitForIdle()
                compose.onNodeWithContentDescription("Derniers messages").assertDoesNotExist()
                history.performTouchInput {
                    advanceEventTime(200)
                    up()
                }
                compose.waitForIdle()
                compose
                    .onNodeWithContentDescription("Derniers messages")
                    .assertIsDisplayed()
                    .performClick()
                waitLast()
                compose.onNodeWithContentDescription("Derniers messages").assertDoesNotExist()
                compose.runOnIdle {
                    owner.registry.currentState = Lifecycle.State.DESTROYED
                    opened = false
                }
                vm.api.closeStreams()
            }
        } finally {
            androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
        }
    }
}
