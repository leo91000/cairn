package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Rule
import org.junit.Test

/** The official session and relayed stream reach the same conversation UI on JVM and device. */
abstract class BackgroundWaitCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun relayedBackgroundWaitReplacesWorking() =
        conversation("running") {
            compose.onNodeWithTag("agent-waiting").assertExists()
            compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
            compose
                .onNodeWithTag("agent-waiting")
                .assert(
                    hasContentDescription(
                        "En attente d’une tâche en arrière-plan : Vérifier la CI. L’agent reprend quand elle se termine."
                    )
                )
            compose.onNodeWithText("Tâche en arrière-plan", substring = true).assertExists()
        }

    @Test
    fun headerWaitTimeAdvancesWithoutStreamEvents() {
        compose.setContent {
            CairnTheme {
                val started = remember { System.currentTimeMillis() - 58_000 }
                ConversationHeader(
                    title = "Conversation de test",
                    agent = "Agent principal",
                    agentKey = MAIN_AGENT_ID,
                    status = "",
                    live = "Tâche en arrière-plan",
                    liveSince = started,
                    back = {},
                    choose = {},
                    actions = {},
                )
            }
        }
        compose
            .onNodeWithText("Tâche en arrière-plan · < 1 min", useUnmergedTree = true)
            .assertExists()
        compose.waitUntil(5000) {
            compose
                .onAllNodesWithText("Tâche en arrière-plan · 1 min", useUnmergedTree = true)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    @Test
    fun failedResponseBannerCanBeDismissed() =
        conversation("failed") {
            val message =
                "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer."
            compose.onNodeWithText(message).assertIsDisplayed()
            compose.onNodeWithText("Échec · Agent principal").assertExists()
            compose.onAllNodesWithTag("agent-waiting").assertCountEquals(0)
            compose.onNodeWithContentDescription("Masquer l’erreur").performClick()
            compose.onNodeWithText(message).assertDoesNotExist()
        }

    private fun conversation(status: String, check: () -> Unit) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val chat =
                Chat(
                    "chat",
                    title = "Conversation de test",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run = Run("run", status = status, startedAt = now - 60_000),
                )
            val event =
                RunEvent(
                    1,
                    now - 30_000,
                    "turn.waiting",
                    "",
                    mapOf(
                        "tasks" to
                            buildJsonArray {
                                addJsonObject { put("description", "Vérifier la CI") }
                            }
                    ),
                )
            val batch =
                wireJson.encodeToString(
                    LiveBatch(listOf(event), LiveState(chat = chat), true, false)
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/installations/fixture/api/chats/chat/stream")
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody("event: batch\nid: 1\ndata: $batch\n\n")

                        val body =
                            when (path) {
                                "/api/account/session" -> officialAccountFixture()
                                "/api/installations" -> officialInstallationsFixture()
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(MAIN_AGENT_ID, "Agent principal"))
                                    )
                                "/api/installations/fixture/api/chats/chat" ->
                                    wireJson.encodeToString(chat)
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
                    BackgroundWaitVault(),
                    officialOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated)
                        ChatScreen(vm, state, "chat", openChat = {}, openRun = {}, back = {})
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Conversation de test")
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    (status != "running" ||
                        compose
                            .onAllNodesWithTag("agent-waiting")
                            .fetchSemanticsNodes()
                            .isNotEmpty())
            }
            try {
                check()
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private class BackgroundWaitVault : SessionVault {
    override fun read(origin: String): String? = null

    override fun write(origin: String, cookie: String?) = Unit
}
