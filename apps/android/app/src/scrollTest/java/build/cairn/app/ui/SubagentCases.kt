package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
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

        compose.onNodeWithContentDescription("Options de la conversation").performClick()
        compose.onNodeWithText("Plein écran").performClick()
        compose.onNodeWithTag("subagent-bar").assertDoesNotExist()
        compose.onNodeWithTag("agent-working").assertIsDisplayed()
        compose.onNodeWithContentDescription("Quitter le plein écran").performClick()
        compose.onNodeWithTag("subagent-bar").assertIsDisplayed()
        compose.onNodeWithTag("agent-working").assertDoesNotExist()
    }

    @Test
    fun workingIndicatorFollowsStatusOnlyUpdates() =
        conversation(withSubagents = false) { update ->
            compose.onNodeWithTag("agent-working").assertIsDisplayed()
            update("queued")
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("agent-working").fetchSemanticsNodes().isEmpty()
            }
            update("running")
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("agent-working").fetchSemanticsNodes().isNotEmpty()
            }
        }

    private fun conversation(
        withSubagents: Boolean = true,
        check: ((String) -> Unit) -> Unit,
    ) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val chat =
                Chat(
                    id = "6cc6efab-10af-54c7-a91b-dcff664166a1",
                    title = "Audit des webhooks",
                    agentId = MAIN_AGENT_ID,
                    agentName = "Agent principal",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            status = "running",
                            startedAt = now - 90_000,
                            taskId = "",
                        ),
                )
            var serial = 0L
            fun event(type: String, at: Long, payload: JsonObject) =
                RunEvent(
                    id = ++serial,
                    createdAt = now - at,
                    type = type,
                    text = "",
                    payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
                    runId = "run",
                )
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
            val subagentEvents =
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
            val events = if (withSubagents) subagentEvents else emptyList()
            val current = AtomicReference(chat)
            val initial = AtomicBoolean(true)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (
                            path ==
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1/stream"
                        ) {
                            val first = initial.getAndSet(false)
                            val batch =
                                wireJson.encodeToString(
                                    timelineBatch(
                                        if (first) events else emptyList(),
                                        LiveState(chat = current.get()),
                                        first,
                                        false,
                                    )
                                )
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody("event: batch\nid: 1\ndata: $batch\n\n")
                        }

                        val body =
                            when (path) {
                                "/api/account/session" -> beaconAccountFixture()
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(
                                            listOf(
                                                Agent(
                                                    exclusiveNodes = emptyList(),
                                                    plugins = emptyList(),
                                                    revision = "",
                                                    createdAt = 0,
                                                    access =
                                                        AccessPolicy(
                                                            mcpTools =
                                                                build.cairn.contracts.McpTools(
                                                                    emptyMap()
                                                                ),
                                                            projects = null,
                                                            skills = null,
                                                            mcps = null,
                                                        ),
                                                    id = MAIN_AGENT_ID,
                                                    name = "Agent principal",
                                                )
                                            )
                                        )
                                    )
                                "/api/installations/fixture/api/chats/6cc6efab-10af-54c7-a91b-dcff664166a1" ->
                                    wireJson.encodeToString(chat)
                                "/api/installations/fixture/api/overview" ->
                                    installationOverviewFixture()
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
                        ChatScreen(
                            vm,
                            state,
                            "6cc6efab-10af-54c7-a91b-dcff664166a1",
                            openChat = {},
                            openRun = {},
                            back = {},
                        )
                }
            }
            compose.waitUntil(20000) {
                val tag = if (withSubagents) "subagent-bar" else "agent-working"
                compose.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
            }
            try {
                check { status -> current.set(chat.copy(run = chat.run!!.copy(status = status))) }
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
