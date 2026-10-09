package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*

abstract class ChatProviderCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun queuedConversationShowsReconnectActionInsteadOfWorking() {
        MockWebServer().use { server ->
            val agent = Agent(MAIN_AGENT_ID, "Agent principal", provider = "claude")
            val run =
                Run(
                    "run",
                    status = "queued",
                    snapshot = Snapshot(agent = agent),
                    accountWaitReason =
                        "Reconnect your Claude Code account in Connections to continue.",
                    accountRequired = "claude",
                    chatExecution = ChatExecution("message"),
                )
            val chat =
                Chat(
                    "chat",
                    title = "Conversation en attente",
                    runId = "run",
                    run = run,
                    messages =
                        listOf(
                            ChatMessage(
                                "message",
                                text = "Améliore cette interface",
                                status = "sending",
                            )
                        ),
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/installations/fixture/api/chats/chat/stream") {
                            val frame =
                                "event: batch\nid: 0\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(chat = chat, run = run), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    java.util.concurrent.TimeUnit.SECONDS,
                                )
                        }
                        val body =
                            when (path) {
                                "/api/installations" -> officialInstallationsFixture()
                                "/api/account/session" -> officialAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(listOf(agent))
                                "/api/installations/fixture/api/claude/models" ->
                                    """{"models":[]}"""
                                "/api/installations/fixture/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            var openedConnections = false
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    ProviderVault(),
                    officialOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated)
                        ChatScreen(
                            vm,
                            state,
                            "chat",
                            openChat = {},
                            openRun = {},
                            openConnections = { openedConnections = true },
                        )
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Compte Claude Code requis", substring = false)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Compte Claude Code requis", substring = false)
                .assertIsDisplayed()
            compose.onNodeWithText("En attente d’un compte Claude Code").assertIsDisplayed()
            compose.onNodeWithText("Améliore cette interface").assertIsDisplayed()
            compose.onNodeWithTag("agent-working").assertDoesNotExist()
            compose.onNodeWithText("Démarrage de l’agent…").assertDoesNotExist()
            compose.onNodeWithText("Ouvrir les connexions").performClick()
            compose.runOnIdle { assertTrue(openedConnections) }
        }
    }

    @Test
    fun chatProviderSurvivesRecreationAndResetsIncompatibleModelBeforeSending() {
        MockWebServer().use { server ->
            val sent = CopyOnWriteArrayList<JsonObject>()
            val agent =
                Agent(MAIN_AGENT_ID, "Agent principal", model = "fixture-deep", reasoning = "ultra")
            val run = Run("run", status = "succeeded", snapshot = Snapshot(agent = agent))
            val chat =
                Chat(
                    "chat",
                    title = "Contexte conservé",
                    runId = "run",
                    agentName = agent.name,
                    run = run,
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/installations/fixture/api/chats/chat/stream") {
                            val frame =
                                "event: batch\nid: 0\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(chat = chat, run = run), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    java.util.concurrent.TimeUnit.SECONDS,
                                )
                        }
                        val body =
                            when (path) {
                                "/api/installations" -> officialInstallationsFixture()
                                "/api/account/session" -> officialAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(listOf(agent))
                                "/api/installations/fixture/api/chats/chat/messages" -> {
                                    sent +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    "{}"
                                }
                                "/api/installations/fixture/api/codex/models" ->
                                    """{"models":[{"model":"fixture-deep","displayName":"Deep thinker","supportedReasoningEfforts":[{"reasoningEffort":"ultra"}]}]}"""
                                "/api/installations/fixture/api/claude/models" ->
                                    """{"models":[{"model":"opus[1m]","displayName":"Opus 1M","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"high"}]}]}"""
                                "/api/installations/fixture/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    ProviderVault(),
                    officialOrigin = "",
                )
            val restoration = StateRestorationTester(compose)
            restoration.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated)
                        ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Deep thinker", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            // Agent, model and effort share a single menu.
            compose.onNodeWithTag("model-picker").performClick()
            compose.onNodeWithText("Codex").assertIsSelected()
            compose.onNodeWithText("Claude Code").performClick()
            compose.onNodeWithText("Claude Code").assertIsSelected()
            compose.onNodeWithText("Deep thinker", substring = true).assertDoesNotExist()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Opus 1M", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText(
                    "Au prochain message, Claude Code reprend le contexte du chat et les fichiers existants."
                )
                .assertExists()
            compose.onNodeWithText("Terminé").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("model-settings-sheet").fetchSemanticsNodes().isEmpty()
            }
            compose
                .onNodeWithTag("model-picker")
                .assert(hasContentDescription("Claude Code", substring = true))
            compose
                .onNode(hasSetTextAction())
                .performTextInput("Continue les modifications existantes")
            restoration.emulateSavedInstanceStateRestore()
            compose
                .onNodeWithTag("model-picker")
                .assert(hasContentDescription("Claude Code · Opus 1M", substring = true))
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").performClick()
            compose.waitUntil(10000) { sent.size == 1 }
            assertEquals("claude", sent[0]["provider"]?.jsonPrimitive?.content)
            assertEquals(
                "Continue les modifications existantes",
                sent[0]["text"]?.jsonPrimitive?.content,
            )
            assertEquals("", sent[0]["model"]?.jsonPrimitive?.content)
            assertEquals("", sent[0]["reasoning"]?.jsonPrimitive?.content)
            assertEquals("queue", sent[0]["mode"]?.jsonPrimitive?.content)
            compose.waitUntil(10000) {
                val field =
                    compose.onAllNodes(hasSetTextAction()).fetchSemanticsNodes().singleOrNull()
                field != null &&
                    field.config[androidx.compose.ui.semantics.SemanticsProperties.EditableText]
                        .text
                        .isEmpty()
            }
            compose.onNodeWithTag("model-picker").performClick()
            compose.onNodeWithText("Codex", substring = false).performClick()
            compose.onNodeWithText("Terminé").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("model-settings-sheet").fetchSemanticsNodes().isEmpty()
            }
            compose.onNode(hasSetTextAction()).performTextInput("Reviens à Codex")
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").performClick()
            compose.waitUntil(10000) { sent.size == 2 }
            assertEquals("codex", sent[1]["provider"]?.jsonPrimitive?.content)
            assertNotEquals(sent[0]["id"], sent[1]["id"])
        }
    }
}

private class ProviderVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
