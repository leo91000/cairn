package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
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
    fun ownerCanEnableAndRevokeInstallationAccessInTheAgentEditor() {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<Agent>()
            val agent = AtomicReference(Agent(MAIN_AGENT_ID, "Agent principal"))
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations/fixture/api/agents/$MAIN_AGENT_ID" &&
                                    request.method == "PUT" -> {
                                    val updated =
                                        wireJson.decodeFromString<Agent>(request.body.readUtf8())
                                    agent.set(updated)
                                    saved += updated
                                    wireJson.encodeToString(updated)
                                }
                                path == "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(listOf(agent.get()))
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/models") -> """{"models":[]}"""
                                path.endsWith("/overview") -> "{}"
                                else -> "[]"
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
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated) ResourcesScreen(vm, state, agents = true)
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithContentDescription("Modifier")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            val label = "Autoriser l’accès à l’installation Cairn courante"
            compose.onNodeWithContentDescription("Modifier").performClick()
            compose.onNodeWithText(label).performScrollTo().assertIsOff().performClick()
            compose.onNodeWithText(label).assertIsOn()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                saved.size == 1 && compose.onAllNodesWithText(label).fetchSemanticsNodes().isEmpty()
            }
            assertTrue(saved[0].access.installation)
            compose.onNodeWithContentDescription("Modifier").performClick()
            compose.onNodeWithText(label).performScrollTo().assertIsOn().performClick()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                saved.size == 2 && compose.onAllNodesWithText(label).fetchSemanticsNodes().isEmpty()
            }
            assertFalse(saved[1].access.installation)
        }
    }
}
