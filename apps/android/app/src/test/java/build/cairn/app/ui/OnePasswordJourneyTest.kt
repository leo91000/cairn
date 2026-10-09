package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsProperties
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
class OnePasswordJourneyTest {
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
    fun `editing access preserves the saved token and sends explicit grants`() {
        journey(OnePasswordAccount(id = "account", name = "Production"), false)
    }

    @Test
    fun `creation masks token input and starts without grants`() {
        journey(OnePasswordAccount(), true)
    }

    private fun journey(initial: OnePasswordAccount, creating: Boolean) {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val result =
                            when (request.path) {
                                "/api/installations" -> officialInstallationsFixture()
                                "/api/account/session" -> officialAccountFixture("fixture-csrf")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(id = MAIN_AGENT_ID, name = "Cairn"))
                                    )
                                "/api/installations/fixture/api/onepassword",
                                "/api/installations/fixture/api/onepassword/account" -> {
                                    if (request.getHeader("X-CSRF-Token") != "fixture-csrf")
                                        return MockResponse().setResponseCode(403)
                                    saved += request.body.readUtf8()
                                    "{}"
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
                    officialOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.agents.isNotEmpty())
                        OnePasswordEditor(vm, state, initial, close = {}) {}
                }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Nom").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Nom").performTextReplacement("Secrets")
            if (creating) {
                compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                compose
                    .onNodeWithText("Token de compte de service")
                    .performTextInput("ops_android_fixture")
                compose
                    .onNode(SemanticsMatcher.keyIsDefined(SemanticsProperties.Password))
                    .assertExists()
            } else {
                compose.onNodeWithText("Cairn").performScrollTo().performClick()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) { saved.isNotEmpty() }
            val payload = wireJson.parseToJsonElement(saved.single()).jsonObject
            assertEquals("Secrets", payload["name"]?.jsonPrimitive?.content)
            if (creating) {
                assertEquals("ops_android_fixture", payload["token"]?.jsonPrimitive?.content)
                assertEquals(JsonArray(emptyList()), payload["agentIds"])
            } else {
                assertFalse(payload.containsKey("token"))
                assertEquals(JsonArray(listOf(JsonPrimitive(MAIN_AGENT_ID))), payload["agentIds"])
            }
        }
    }
}
