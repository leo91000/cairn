package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ChatSwipeTest {
    @get:Rule val compose = createComposeRule()
    private var incomingChat by mutableStateOf("")
    private val restoration = StateRestorationTester(compose)
    private val pager
        get() = compose.onNodeWithTag("conversation-pager")

    private val history
        get() = compose.onNodeWithTag("conversation-history")

    private lateinit var vm: CairnViewModel

    @Before
    fun setup() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun cleanup() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun swipesNavigateInSelectorOrderPreserveDraftsAndRespectEnds() = withChats {
        open("Chat récent")
        compose.onNode(hasSetTextAction()).performTextInput("Brouillon récent")
        history.performTouchInput { swipeRight() }
        title("Chat récent").assertExists()
        history.performTouchInput { swipeLeft() }
        waitForChat("Chat intermédiaire")
        compose.onNode(hasSetTextAction()).performTextInput("Brouillon intermédiaire")
        restoration.emulateSavedInstanceStateRestore()
        waitForChat("Chat intermédiaire")
        compose.onNode(hasSetTextAction() and hasText("Brouillon intermédiaire")).assertExists()
        history.performTouchInput { swipeLeft() }
        waitForChat("Chat ancien")
        history.performTouchInput { swipeLeft() }
        title("Chat ancien").assertExists()
        history.performTouchInput { swipeRight() }
        waitForChat("Chat intermédiaire")
        compose.onNode(hasSetTextAction() and hasText("Brouillon intermédiaire")).assertExists()
        history.performTouchInput { swipeRight() }
        waitForChat("Chat récent")
        compose.onNode(hasSetTextAction() and hasText("Brouillon récent")).assertExists()
        // Switching replaces the detail route: Back returns directly to Fil.
        compose.onNodeWithContentDescription("Retour").performClick()
        compose.onNodeWithTag("fil").assertExists()
    }

    @Test
    fun verticalShortCancelledAndComposerGesturesDoNotNavigate() = withChats {
        open("Chat intermédiaire")
        val beforeScroll =
            history.fetchSemanticsNode().config[SemanticsProperties.VerticalScrollAxisRange].value()
        history.performTouchInput { swipeDown() }
        title("Chat intermédiaire").assertExists()
        Assert.assertTrue(
            history
                .fetchSemanticsNode()
                .config[SemanticsProperties.VerticalScrollAxisRange]
                .value() < beforeScroll
        )
        history.performTouchInput { swipe(center, center + Offset(-35f, 0f)) }
        title("Chat intermédiaire").assertExists()
        history.performTouchInput {
            down(Offset(width * .85f, centerY))
            moveTo(Offset(width * .65f, centerY), 300)
            cancel()
        }
        title("Chat intermédiaire").assertExists()
        compose.onNode(hasSetTextAction()).performTouchInput { swipeLeft() }
        title("Chat intermédiaire").assertExists()
        // A cancelled gesture must not leak its accumulated distance into the next one.
        history.performTouchInput { swipeRight() }
        waitForChat("Chat récent")
    }

    @Test
    fun fingerRevealsTheActualNeighborBeforeReleaseAndCanReturnToTheCurrentChat() = withChats {
        open("Chat récent")
        compose.waitUntil(10000) { hasReply("Chat intermédiaire", visible = false) }
        val bounds = pager.getUnclippedBoundsInRoot()
        val width = bounds.right.value - bounds.left.value
        pager.performTouchInput {
            down(Offset(this.width * .90f, height * .4f))
            moveTo(Offset(this.width * .48f, height * .4f), 350)
        }
        compose.waitForIdle()
        val current = compose.onNodeWithTag("chat-page:recent").getUnclippedBoundsInRoot()
        val next = compose.onNodeWithTag("chat-page:middle").getUnclippedBoundsInRoot()
        Assert.assertTrue(
            "The current screen follows the finger",
            current.left.value < -width * .25f,
        )
        Assert.assertTrue("The next screen is already visible", next.left.value in 0f..width * .85f)
        Assert.assertEquals("Pages stay side by side", current.right.value, next.left.value, 1f)
        Assert.assertTrue(
            "The preview contains the real reply",
            hasReply("Chat intermédiaire", visible = true),
        )
        title("Chat récent").assertExists()
        screenshot("chat-swipe-preview-left")
        // Change your mind without lifting: the same screens follow the finger back.
        pager.performTouchInput {
            moveTo(Offset(this.width * .88f, height * .4f), 350)
            up()
        }
        waitForChat("Chat récent")
        Assert.assertEquals(
            0f,
            compose.onNodeWithTag("chat-page:recent").getUnclippedBoundsInRoot().left.value,
            1f,
        )
        history.performTouchInput { swipeLeft() }
        waitForChat("Chat intermédiaire")
        pager.performTouchInput {
            down(Offset(this.width * .10f, height * .4f))
            moveTo(Offset(this.width * .52f, height * .4f), 350)
        }
        compose.waitForIdle()
        val previous = compose.onNodeWithTag("chat-page:recent").getUnclippedBoundsInRoot()
        val middle = compose.onNodeWithTag("chat-page:middle").getUnclippedBoundsInRoot()
        Assert.assertTrue(
            "The previous screen is visible while swiping right",
            previous.right.value in 1f..width * .85f,
        )
        Assert.assertEquals(previous.right.value, middle.left.value, 1f)
        Assert.assertTrue(hasReply("Chat récent", visible = true))
        screenshot("chat-swipe-preview-right")
        pager.performTouchInput {
            moveTo(Offset(this.width * .12f, height * .4f), 350)
            up()
        }
        waitForChat("Chat intermédiaire")
    }

    private fun hasReply(chat: String, visible: Boolean): Boolean = compose.runOnIdle {
        val activity =
            androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry.getInstance()
                .getActivitiesInStage(androidx.test.runner.lifecycle.Stage.RESUMED)
                .first()
        fun descendants(view: android.view.View): Sequence<android.view.View> = sequence {
            yield(view)
            if (view is android.view.ViewGroup)
                for (index in 0 until view.childCount) yieldAll(descendants(view.getChildAt(index)))
        }
        descendants(activity.window.decorView).filterIsInstance<android.widget.TextView>().any {
            it.text.contains("de $chat") &&
                (!visible || it.getGlobalVisibleRect(android.graphics.Rect()))
        }
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        File(dir, "$name.png").outputStream().use {
            compose
                .onRoot()
                .captureToImage()
                .asAndroidBitmap()
                .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }

    @Test
    fun onlyTheSelectedChatKeepsALiveStreamWhileNeighborsStayPreviewed() = withChats {
        open("Chat récent")
        compose.waitUntil(10000) { hasReply("Chat intermédiaire", visible = false) }
        waitForChatStreams("recent")
        history.performTouchInput { swipeLeft() }
        waitForChat("Chat intermédiaire")
        compose.waitUntil(10000) {
            hasReply("Chat ancien", visible = false) && hasReply("Chat récent", visible = false)
        }
        waitForChatStreams("middle")
    }

    /** Previews release their catch-up stream; only the settled page may stay connected. */
    private fun waitForChatStreams(vararg expected: String) {
        fun open() =
            vm.api.streamCalls
                .map { it.request().url.encodedPath }
                .filter {
                    it.startsWith("/api/installations/fixture/api/chats/") &&
                        it != "/api/installations/fixture/api/chats/stream"
                }
                .map {
                    it.removePrefix("/api/installations/fixture/api/chats/").removeSuffix("/stream")
                }
                .sorted()
        compose.waitUntil(10000) { open() == expected.sorted() }
        // Stay settled: no preview reconnects behind the selected chat.
        Thread.sleep(1500)
        compose.waitForIdle()
        Assert.assertEquals(expected.sorted(), open())
    }

    @Test
    fun notificationReopensItsRequestedChatAfterSwipingAwayFromIt() = withChats {
        open("Chat récent")
        history.performTouchInput { swipeLeft() }
        waitForChat("Chat intermédiaire")
        compose.runOnIdle { incomingChat = "recent" }
        waitForChat("Chat récent")
        compose.onNodeWithContentDescription("Retour").performClick()
        compose.onNodeWithTag("fil").assertExists()
    }

    @Test
    fun singleAndNewConversationsDoNotNavigate() =
        withChats(single = true) {
            open("Chat récent")
            history.performTouchInput {
                swipeLeft()
                swipeRight()
            }
            title("Chat récent").assertExists()
            compose.onNodeWithContentDescription("Retour").performClick()
            compose.onNodeWithContentDescription("Nouvelle conversation").performClick()
            history.performTouchInput {
                swipeLeft()
                swipeRight()
            }
            title("Nouvelle conversation").assertExists()
        }

    private fun title(name: String) =
        compose.onNodeWithContentDescription("Changer de conversation : $name")

    private fun waitForChat(name: String) {
        compose.waitUntil(10000) {
            compose
                .onAllNodesWithContentDescription("Changer de conversation : $name")
                .fetchSemanticsNodes()
                .isNotEmpty() &&
                compose
                    .onAllNodesWithTag("conversation-history")
                    .fetchSemanticsNodes()
                    .singleOrNull()
                    ?.config
                    ?.getOrNull(SemanticsProperties.VerticalScrollAxisRange)
                    ?.value()
                    ?.let { it > 0f } == true
        }
        compose.waitForIdle()
    }

    private fun open(name: String) {
        compose.waitUntil(10000) {
            compose.onAllNodesWithText(name).fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithText(name).performClick()
        waitForChat(name)
    }

    private fun withChats(single: Boolean = false, test: () -> Unit) {
        MockWebServer().use { server ->
            val agent = Agent(MAIN_AGENT_ID, "Agent principal")
            val chats =
                if (single) listOf(Chat("recent", "Chat récent", updatedAt = 300))
                else
                    listOf(
                        Chat("old", "Chat ancien", updatedAt = 100),
                        Chat("recent", "Chat récent", updatedAt = 300),
                        Chat("middle", "Chat intermédiaire", updatedAt = 200),
                    )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        if (path.endsWith("/stream")) {
                            val chat = chats.find {
                                path == "/api/installations/fixture/api/chats/${it.id}/stream"
                            }
                            val events =
                                if (chat == null) emptyList()
                                else
                                    (1..30).map {
                                        RunEvent(
                                            it.toLong(),
                                            it.toLong(),
                                            "item.completed",
                                            "",
                                            mapOf(
                                                "item" to
                                                    buildJsonObject {
                                                        put("id", "answer-$it")
                                                        put("type", "agent_message")
                                                        put(
                                                            "text",
                                                            "Réponse **$it** de ${chat.title}. " +
                                                                "Du contenu sélectionnable pour faire défiler l’historique. "
                                                                    .repeat(12),
                                                        )
                                                    }
                                            ),
                                        )
                                    }
                            val state =
                                if (chat == null) LiveState(chats = chats)
                                else LiveState(chat = chat)
                            val frame =
                                "event: batch\nid: ${events.lastOrNull()?.id ?: 0}\ndata: ${wireJson.encodeToString(LiveBatch(events, state, false, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    TimeUnit.SECONDS,
                                )
                        }
                        val body =
                            when (path) {
                                "/api/installations" -> officialInstallationsFixture()
                                "/api/account/session" -> officialAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(listOf(agent))
                                "/api/installations/fixture/api/codex/models",
                                "/api/installations/fixture/api/claude/models" ->
                                    """{"models":[]}"""
                                "/api/installations/fixture/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    override fun read(origin: String) =
                        "cairn_session=fixture; Path=/; Max-Age=3600"

                    override fun write(origin: String, cookie: String?) = Unit
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    officialOrigin = "",
                )
            this.vm = vm
            restoration.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    CairnApp(
                        vm = vm,
                        targetChat = incomingChat,
                        targetCacheScope =
                            if (state.installation != null) vm.api.cacheScope else "",
                        consumedTarget = { incomingChat = "" },
                    )
                }
            }
            try {
                test()
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
