package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
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

abstract class McpJourneyCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun editingStdioPreservesMultilineArgumentsBlankArgumentsAndSecrets() {
        MockWebServer().use { server ->
            val initial =
                Mcp(
                    id = "00000000-0000-4000-8000-000000000001",
                    checkedAt = null,
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
                                                    name = "Cairn",
                                                )
                                            )
                                        )
                                    )
                                "/api/installations/fixture/api/mcps" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListMcpsOutput(listOf(initial))
                                    )
                                "/api/installations/fixture/api/mcps/${initial.id}" -> {
                                    if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                                        return MockResponse().setResponseCode(403)
                                    saved += request.body.readUtf8()
                                    wireJson.encodeToString(initial)
                                }
                                "/api/installations/fixture/api/overview",
                                "/api/installations/fixture/api/codex/models" -> "{}"
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
                                        request.requestUrl?.encodedPath?.endsWith("/api/mcps") ==
                                            true
                                    )
                                        """{"mcps":[]}"""
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(result)
                    }
                }
            server.start()
            val store = androidx.lifecycle.ViewModelStore()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        private val cookies = mutableMapOf<String, String>()

                        override fun read(origin: String) = cookies[origin]

                        override fun write(origin: String, cookie: String?) {
                            if (cookie == null) cookies.remove(origin) else cookies[origin] = cookie
                        }
                    },
                    beaconOrigin = "",
                )
            store.put("fixture", vm)
            var mounted by mutableStateOf(true)
            compose.setContent {
                if (!mounted) return@setContent
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme { if (state.agents.isNotEmpty()) McpEditor(vm, state, initial) {} }
            }
            try {
                compose.waitUntil(10000) {
                    compose.onAllNodesWithText("Nom").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Nom").performTextReplacement("Outil renommé")
                compose.onNodeWithText("Enregistrer").performClick()
                compose.waitUntil(10000) { saved.isNotEmpty() }
                val command = wireJson.parseToJsonElement(saved.single()).jsonObject
                assertEquals(0L, command["revision"]?.jsonPrimitive?.long)
                val payload = command["connection"]!!.jsonObject
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
            } finally {
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                store.clear()
            }
        }
    }
}
