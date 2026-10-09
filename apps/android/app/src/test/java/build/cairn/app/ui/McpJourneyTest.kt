package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.*
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
class McpJourneyTest {
    @get:Rule val compose = createComposeRule()

    @Before
    fun initializeWork() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun closeWork() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun `editing a stdio connection preserves multiline arguments blank arguments and existing secrets`() {
        MockWebServer().use { server ->
            val initial =
                Mcp(
                    id = "mcp",
                    name = "Mon outil",
                    transport = "stdio",
                    command = "node",
                    args = listOf("-e", "const value = 1;\nconsole.log(value);", ""),
                    envKeys = listOf("TOKEN"),
                    enabledTools = listOf("read"),
                )
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val result =
                            when (request.path) {
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture-csrf")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(id = MAIN_AGENT_ID, name = "Cairn"))
                                    )
                                "/api/installations/fixture/api/mcps" ->
                                    wireJson.encodeToString(listOf(initial))
                                "/api/installations/fixture/api/mcps/mcp" -> {
                                    if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                                        return MockResponse().setResponseCode(403)
                                    saved += request.body.readUtf8()
                                    wireJson.encodeToString(initial)
                                }
                                "/api/installations/fixture/api/overview",
                                "/api/installations/fixture/api/codex/models" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(result)
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme { if (state.agents.isNotEmpty()) McpEditor(vm, state, initial) {} }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Nom").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Nom").performTextReplacement("Outil renommé")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Outil renommé", payload["name"]?.jsonPrimitive?.content)
            assertEquals(wireJson.encodeToJsonElement(initial.args), payload["args"])
            assertEquals(
                wireJson.encodeToJsonElement(initial.enabledTools),
                payload["enabledTools"],
            )
            assertEquals(JsonObject(emptyMap()), payload["env"])
            assertEquals(JsonArray(emptyList()), payload["removeEnv"])
            assertFalse(payload.containsKey("token"))
            assertFalse(payload.containsKey("clientSecret"))
        }
    }
}
