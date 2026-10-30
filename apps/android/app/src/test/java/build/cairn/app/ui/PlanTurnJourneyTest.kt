package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

// The generated message inputs require a UUID conversation id.
private const val CHAT = "00000000-0000-4000-8000-0000000000c1"

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PlanTurnJourneyTest {
    @get:Rule val compose = createComposeRule()

    @org.junit.Before
    fun initializeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @org.junit.After
    fun closeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun `plan requests show their badge and implementing the proposed plan sends an ordinary message`() {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    id = CHAT,
                    title = "Composeur",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            taskId = CHAT,
                            status = "succeeded",
                            trigger = "chat",
                            snapshot =
                                Snapshot(
                                    task =
                                        build.cairn.contracts.RunTaskSnapshot(
                                            agentId = MAIN_AGENT_ID,
                                            id = "00000000-0000-4000-8000-000000000000",
                                            name = "Composeur",
                                            prompt = "",
                                        ),
                                    agent = newAgent().copy(provider = "claude"),
                                ),
                            chatExecution = ChatExecution(messageId = "plan-request", plan = true),
                        ),
                )
            val events =
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = 1789315189000,
                        type = "chat.user",
                        text = "Planifie le nouveau composeur",
                        payload =
                            buildJsonObject {
                                put("text", "Planifie le nouveau composeur")
                                put("plan", true)
                            },
                    ),
                    RunEvent(id = 2, createdAt = 1789315190000, type = "turn.started", text = ""),
                    RunEvent(
                        id = 3,
                        createdAt = 1789315191000,
                        type = "item.completed",
                        text = "",
                        payload =
                            buildJsonObject {
                                put(
                                    "item",
                                    buildJsonObject {
                                        put("id", "exit-plan-tool")
                                        put("type", "proposed_plan")
                                        put(
                                            "text",
                                            "1. Lire le composeur\n2. Ajouter le bouton Plan",
                                        )
                                        put("status", "completed")
                                    },
                                )
                            },
                    ),
                    RunEvent(id = 4, createdAt = 1789315192000, type = "turn.completed", text = ""),
                )
            val vm = mountPlanChat(server, chat, events, calls)
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("proposed-plan").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Plan proposé").assertExists()
            compose.onNodeWithText("Plan · ", substring = true).assertExists()
            screenshot("plan-turn")

            // A follow-up question stays a plan turn while the toggle is on.
            compose.onNodeWithTag("plan-toggle").assertIsEnabled().performClick()
            compose.onNodeWithTag("plan-toggle").assertIsSelected()
            compose.onNode(hasSetTextAction()).performTextInput("Et pour les tests ?")
            compose.onNodeWithTag("conversation-send").performClick()
            compose.waitUntil(20000) { calls.size == 1 }
            val question = wireJson.parseToJsonElement(calls[0]).jsonObject
            assertEquals(true, question["plan"]?.jsonPrimitive?.boolean)
            assertEquals("Et pour les tests ?", question["text"]?.jsonPrimitive?.content)

            // Implementing is explicit: an ordinary message in the same conversation.
            compose.waitUntil(20000) {
                compose
                    .onAllNodes(hasTestTag("implement-plan") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("implement-plan").performClick()
            compose.waitUntil(20000) { calls.size == 2 }
            val implementing = wireJson.parseToJsonElement(calls[1]).jsonObject
            assertEquals(false, implementing["plan"]?.jsonPrimitive?.boolean)
            assertEquals(
                "Implémente le plan que tu as proposé.",
                implementing["text"]?.jsonPrimitive?.content,
            )
            compose.onNodeWithTag("plan-toggle").assertIsNotSelected()
            vm.api.closeStreams()
        }
    }

    @Test
    fun `saved Plan intent cannot become ordinary with OpenCode`() {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    id = CHAT,
                    title = "Intention Plan",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            taskId = CHAT,
                            status = "succeeded",
                            trigger = "chat",
                            snapshot =
                                Snapshot(
                                    task =
                                        build.cairn.contracts.RunTaskSnapshot(
                                            agentId = MAIN_AGENT_ID,
                                            id = "00000000-0000-4000-8000-000000000000",
                                            name = "Intention Plan",
                                            prompt = "",
                                        ),
                                    agent = newAgent().copy(provider = "opencode"),
                                ),
                            chatExecution = ChatExecution(messageId = "ordinary-before-plan"),
                        ),
                )
            val vm =
                mountPlanChat(
                    server,
                    chat,
                    emptyList(),
                    calls,
                    ChatDraft(text = "Examiner sans modifier", provider = "opencode", plan = true),
                )
            try {
                compose.waitUntil(20_000) {
                    compose.onAllNodesWithTag("plan-toggle").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected().assertIsEnabled()
                compose.onNodeWithTag("plan-unavailable").assertIsDisplayed()
                compose.onNodeWithContentDescription("Envoyer").assertIsNotEnabled()
                assertTrue(calls.isEmpty())

                compose.onNodeWithTag("model-picker").performClick()
                compose.onNode(hasText("Codex") and isSelectable()).performClick()
                compose.onNodeWithText("Terminé").performClick()
                compose.waitUntil(20_000) {
                    compose
                        .onAllNodesWithTag("model-settings-sheet")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected()
                compose.onNodeWithTag("conversation-send").assertIsEnabled().performClick()
                compose.waitUntil(20_000) { calls.isNotEmpty() || vm.state.value.error != null }
                assertNull(
                    "Submission failed: ${vm.state.value.error}; calls=$calls",
                    vm.state.value.error,
                )
                assertEquals(1, calls.size)
                compose.waitUntil(20_000) {
                    compose.onAllNodesWithTag("conversation-send").fetchSemanticsNodes()
                    !vm.state.value.busy
                }
                val planned = wireJson.parseToJsonElement(calls[0]).jsonObject
                assertEquals(true, planned["plan"]?.jsonPrimitive?.boolean)
                assertEquals("codex", planned["provider"]?.jsonPrimitive?.content)

                compose.onNode(hasSetTextAction()).performTextInput("Demande ordinaire explicite")
                compose.onNodeWithTag("model-picker").performClick()
                compose.onNode(hasText("OpenCode") and isSelectable()).performClick()
                compose.onNodeWithText("Terminé").performClick()
                compose.waitUntil(20_000) {
                    compose
                        .onAllNodesWithTag("model-settings-sheet")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithTag("plan-toggle").assertIsSelected()
                compose.onNodeWithContentDescription("Envoyer").assertIsNotEnabled()
                assertEquals(1, calls.size)
                compose.onNodeWithTag("plan-toggle").performClick()
                compose.onNodeWithTag("plan-toggle").assertIsNotSelected()
                compose.onNodeWithTag("conversation-send").assertIsEnabled().performClick()
                compose.waitUntil(20_000) { calls.size == 2 }
                val ordinary = wireJson.parseToJsonElement(calls[1]).jsonObject
                assertEquals(false, ordinary["plan"]?.jsonPrimitive?.boolean)
                assertEquals("opencode", ordinary["provider"]?.jsonPrimitive?.content)
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun mountPlanChat(
        server: MockWebServer,
        chat: Chat,
        events: List<RunEvent>,
        calls: CopyOnWriteArrayList<String>,
        savedDraft: ChatDraft? = null,
    ): CairnViewModel {
        val batch = wireJson.encodeToString(LiveBatch(events, LiveState(chat = chat), true, false))
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path!!.substringBefore('?')
                    if (path == "/api/installations/fixture/api/chats/$CHAT/stream") {
                        val initial = "event: batch\nid: 4\ndata: $batch\n\n"
                        return MockResponse()
                            .setHeader("Content-Type", "text/event-stream")
                            .setBody(initial + ": keepalive\n".repeat(100_000))
                            .throttleBody(
                                initial.toByteArray().size.toLong(),
                                250,
                                TimeUnit.MILLISECONDS,
                            )
                    }
                    if (path == "/api/installations/fixture/api/chats/$CHAT/messages") {
                        val sent = request.body.readUtf8()
                        calls += sent
                        // The accepted message echoes its id, like the installation.
                        val id = wireJson.parseToJsonElement(sent).jsonObject["id"]
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(buildJsonObject { put("id", id!!) }.toString())
                    }
                    val body =
                        when (path) {
                            "/api/account/session" -> beaconAccountFixture()
                            "/api/installations" -> beaconInstallationsFixture()
                            "/api/installations/fixture/api/agents" ->
                                wireJson.encodeToString(
                                    build.cairn.contracts.ListAgentsOutput(
                                        listOf(
                                            newAgent()
                                                .copy(
                                                    id = MAIN_AGENT_ID,
                                                    name = "Agent principal",
                                                    provider =
                                                        chat.run?.snapshot?.agent?.provider
                                                            ?: "claude",
                                                )
                                        )
                                    )
                                )
                            "/api/installations/fixture/api/chats/$CHAT" ->
                                wireJson.encodeToString(chat)
                            "/api/installations/fixture/api/projects" -> """{"projects":[]}"""
                            "/api/installations/fixture/api/tasks" -> """{"tasks":[]}"""
                            "/api/installations/fixture/api/mcps" -> """{"mcps":[]}"""
                            "/api/installations/fixture/api/overview" -> "{}"
                            else -> "[]"
                        }
                    return MockResponse()
                        .setHeader("Content-Type", "application/json")
                        .setBody(body)
                }
            }
        server.start()
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                MemoryVault().apply {
                    write(
                        server.url("/").toString(),
                        "cairn_session=fixture; Path=/; Max-Age=3600",
                    )
                },
                beaconOrigin = "",
            )
        val connected = mutableStateOf(false)
        compose.setContent {
            val state by vm.state.collectAsStateWithLifecycle()
            LaunchedEffect(Unit) {
                vm.state.first { it.ready && !it.busy }
                vm.connect(server.url("/").toString())
                if (savedDraft != null) vm.chatDrafts[CHAT] = savedDraft
                connected.value = true
            }
            CairnTheme(preference = "system") {
                if (state.session.authenticated && connected.value)
                    Surface(
                        Modifier.fillMaxSize(),
                        color = MaterialTheme.colorScheme.background,
                    ) {
                        Box(Modifier.safeDrawingPadding()) {
                            ChatScreen(
                                vm,
                                state,
                                CHAT,
                                openChat = {},
                                openRun = {},
                                back = {},
                            )
                        }
                    }
            }
        }
        return vm
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        compose.onRoot().captureToImage().asAndroidBitmap().let { bitmap ->
            File(dir, "$name.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }
}
