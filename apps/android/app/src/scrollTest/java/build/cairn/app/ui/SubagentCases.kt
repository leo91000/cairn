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

/** A conversation whose agent waits for its subagents, on JVM and device. */
abstract class SubagentCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) = Unit

    @Test
    fun workingSubagentsStayInSightAndOpenTheirActivity() = conversation {
        compose.onNodeWithTag("subagent-team").assertExists()
        compose
            .onNode(hasText("2 en cours") and hasAnyAncestor(hasTestTag("subagent-team")))
            .assertExists()
        // The bar names the subagents the agent waits for, instead of the working indicator.
        compose.onNodeWithTag("subagent-bar").assertIsDisplayed()
        compose.onAllNodesWithTag("agent-working").assertCountEquals(0)
        // Their steps and messages stay out of the agent's own timeline.
        compose.onAllNodesWithTag("agent-actions").assertCountEquals(0)
        capture("subagent-conversation")

        compose
            .onNodeWithContentDescription("2 sous-agents au travail. Ouvrir leur activité")
            .performClick()
        compose.waitUntil(5000) {
            compose.onAllNodesWithTag("subagent-sheet").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithText("Rechercher dans les fichiers").assertIsDisplayed()
        compose.onNodeWithText("Dernier message").assertIsDisplayed()
        capture("subagent-sheet")

        compose.onNodeWithContentDescription("Fermer l’activité du sous-agent").performClick()
        compose.waitUntil(5000) {
            compose.onAllNodesWithTag("subagent-sheet").fetchSemanticsNodes().isEmpty()
        }
    }

    private fun conversation(check: () -> Unit) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val chat =
                Chat(
                    "chat",
                    title = "Audit des webhooks",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run = Run("run", status = "running", startedAt = now - 90_000),
                )
            var serial = 0L
            fun event(type: String, at: Long, payload: JsonObject) =
                RunEvent(++serial, now - at, type, "", payload, runId = "run")
            fun item(type: String, at: Long, body: JsonObjectBuilder.() -> Unit) =
                event(type, at, buildJsonObject { put("item", buildJsonObject(body)) })
            fun subagent(name: String, at: Long, prompt: String) =
                item("item.started", at) {
                    put("id", name)
                    put("type", "subagent")
                    put("name", name)
                    put("agent_type", "Explore")
                    put("prompt", prompt)
                    put("status", "in_progress")
                }
            val events =
                listOf(
                    event(
                        "chat.user",
                        85_000,
                        buildJsonObject {
                            put("text", "Audite les webhooks et les tests de facturation")
                            put("messageId", "audit")
                        },
                    ),
                    item("item.completed", 80_000) {
                        put("id", "intro")
                        put("type", "agent_message")
                        put("text", "Je lance deux sous-agents en parallèle.")
                    },
                    subagent("Webhooks", 75_000, "Trouve chaque gestionnaire de webhook."),
                    subagent("Tests de facturation", 74_000, "Lance les tests de facturation."),
                    item("item.completed", 60_000) {
                        put("id", "grep")
                        put("type", "command_execution")
                        put("command", "rg -n webhook crates")
                        put("exit_code", 0)
                        put("parent_id", "Webhooks")
                    },
                    item("item.completed", 50_000) {
                        put("id", "note")
                        put("type", "subagent_message")
                        put("text", "Trois gestionnaires trouvés.")
                        put("parent_id", "Webhooks")
                    },
                )
            val batch =
                wireJson.encodeToString(LiveBatch(events, LiveState(chat = chat), true, false))
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
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
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
                    SubagentVault(),
                    beaconOrigin = "",
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
                compose.onAllNodesWithTag("subagent-bar").fetchSemanticsNodes().isNotEmpty()
            }
            try {
                check()
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}

private class SubagentVault : SessionVault {
    override fun read(origin: String): String? = null

    override fun write(origin: String, cookie: String?) = Unit
}
