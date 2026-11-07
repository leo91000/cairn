package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import build.cairn.contracts.ReplaceAgentInput
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class InstallationAccessCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun ownerCanEnableAndRevokeInstallationAccessOnTheAgentPage() {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<Agent>()
            val agent =
                AtomicReference(
                    Agent(
                        exclusiveNodes = emptyList(),
                        plugins = emptyList(),
                        revision = "",
                        createdAt = 0,
                        access =
                            AccessPolicy(
                                mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                projects = null,
                                skills = null,
                                mcps = null,
                            ),
                        id = MAIN_AGENT_ID,
                        name = "Agent principal",
                    )
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations/fixture/api/agents/$MAIN_AGENT_ID" &&
                                    request.method == "PUT" -> {
                                    val command =
                                        wireJson.decodeFromString<ReplaceAgentInput>(
                                            request.body.readUtf8()
                                        )
                                    val updated =
                                        agentFixture(command.settings, command.agentId, "saved")
                                    agent.set(updated)
                                    saved += updated
                                    wireJson.encodeToString(updated)
                                }
                                path.endsWith("/preview") ->
                                    """{"runs":{"queued":0,"running":0},"delegations":[]}"""
                                path == "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(agent.get()))
                                    )
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/models") -> """{"models":[]}"""
                                path.endsWith("/overview") -> installationOverviewFixture()
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
            val vault =
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) {}
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var mounted by mutableStateOf(true)
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (mounted && state.agents.isNotEmpty())
                        AgentScreen(vm, state, MAIN_AGENT_ID, back = {}, chat = {}, navigate = {})
                }
            }
            try {
                grantAndRevoke(saved)
            } finally {
                // Unmount and clear the view model before the server closes.
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
            }
        }
    }

    private fun grantAndRevoke(saved: List<Agent>) {
        compose.waitUntil(20000) {
            compose.onAllNodesWithTag("agent-section-Powers").fetchSemanticsNodes().isNotEmpty()
        }
        val label = "Gérer l’installation Cairn"
        compose.onNodeWithTag("agent-section-Powers").performScrollTo().performClick()
        compose.onNodeWithText(label).assertIsOff().performClick()
        compose.onNodeWithText(label).assertIsOn()
        compose.onNodeWithContentDescription("Retour").performClick()
        compose.onNodeWithText("Enregistrer").performClick()
        // A power added is always reviewed before saving.
        compose.waitUntil(10000) {
            compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
        }
        compose.onNodeWithText("Ajout : gestion de l’installation", substring = true).assertExists()
        compose
            .onNode(hasText("Enregistrer") and hasAnyAncestor(hasTestTag("agent-review")))
            .performClick()
        compose.waitUntil(10000) {
            saved.size == 1 &&
                compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
        }
        assertTrue(saved[0].access.installation)
        compose.onNodeWithTag("agent-section-Powers").performScrollTo().performClick()
        compose.onNodeWithText(label).assertIsOn().performClick()
        compose.onNodeWithContentDescription("Retour").performClick()
        // Removing a power that stops no work saves directly.
        compose.onNodeWithText("Enregistrer").performClick()
        compose.waitUntil(10000) {
            saved.size == 2 &&
                compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
        }
        assertFalse(saved[1].access.installation)
    }
}
