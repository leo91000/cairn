package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.statusBars
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.TimeUnit
import kotlin.math.abs
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

/**
 * Issue #176: one header per screen, the status bar inset applied once, and the Connexion sheet.
 */
abstract class UnifiedHeaderCases {
    protected open fun capture(name: String) {}

    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val chat =
        Chat(
            "chat",
            "Orchestration de la release",
            agentName = "Agent principal",
            projectName = "Cairn",
            runId = "run",
            run = Run("run", status = "succeeded", startedAt = now - 60_000),
            updatedAt = now,
        )
    private val installations =
        """[{"id":"fixture","name":"Maison","role":"owner","online":true},""" +
            """{"id":"shared","name":"Atelier partagé","role":"member","online":true}]"""

    private fun stream(state: LiveState): MockResponse {
        val frame =
            "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    private fun fixture(server: MockWebServer): CairnViewModel {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.path!!.substringBefore('?')
                    when (path) {
                        "/api/installations/fixture/api/chats/stream" ->
                            return stream(LiveState(chats = listOf(chat)))
                        "/api/installations/fixture/api/chats/chat/stream" ->
                            return stream(
                                LiveState(
                                    chat = chat,
                                    artifacts =
                                        (1..4).map {
                                            Deliverable("a$it", "run", key = "a$it", name = "a$it")
                                        },
                                )
                            )
                    }
                    val body =
                        when (path) {
                            "/api/installations" -> installations
                            "/api/account/session" ->
                                """{"authenticated":true,"csrf":"fixture","account":{"id":"fixture-account","email":"owner@example.test"},"installations":$installations}"""
                            "/api/installations/fixture/api/agents" ->
                                wireJson.encodeToString(
                                    listOf(Agent(MAIN_AGENT_ID, "Agent principal"))
                                )
                            "/api/installations/fixture/api/projects" ->
                                wireJson.encodeToString(listOf(Project("project", "Cairn")))
                            "/api/installations/fixture/api/tasks",
                            "/api/installations/fixture/api/tasks/activity",
                            "/api/installations/fixture/api/skills",
                            "/api/installations/fixture/api/mcps" -> "[]"
                            else -> "{}"
                        }
                    return MockResponse()
                        .setHeader("Content-Type", "application/json")
                        .setBody(body)
                }
            }
        val vault =
            object : SessionVault {
                private val values = mutableMapOf<String, String>()

                override fun read(origin: String) = values[origin]

                override fun write(origin: String, cookie: String?) {
                    if (cookie == null) values.remove(origin) else values[origin] = cookie
                }
            }
        val application = ApplicationProvider.getApplicationContext<Application>()
        return CairnViewModel(application, vault, beaconOrigin = "")
    }

    /**
     * Connects from composition: blocking the test thread would stall Robolectric's main looper.
     */
    private fun setApp(vm: CairnViewModel, server: MockWebServer, dark: Boolean = true) {
        compose.setContent {
            val local = LocalDensity.current
            val inset = WindowInsets.statusBars.getTop(local).toFloat()
            SideEffect {
                statusBar = inset
                density = local.density
            }
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                vm.connect(server.url("/").toString())
            }
            CairnTheme(if (dark) "dark" else "light") { CairnApp(vm = vm) }
        }
    }

    private var statusBar = 0f
    private var density = 1f

    private fun waitText(value: String) =
        compose.waitUntil(30000) {
            compose.onAllNodesWithText(value).fetchSemanticsNodes().isNotEmpty()
        }

    private fun waitDescription(value: String) =
        compose.waitUntil(30000) {
            compose.onAllNodesWithContentDescription(value).fetchSemanticsNodes().isNotEmpty()
        }

    /** The first content sits right under the status bar: its inset is applied exactly once. */
    private fun assertTopsUnderStatusBar(node: SemanticsNodeInteraction, statusBar: Float) {
        val top = node.fetchSemanticsNode().boundsInRoot.top
        assertTrue("Top $top should follow the $statusBar px status bar", abs(top - statusBar) < 2f)
    }

    @Test
    fun filAndConversationShowOneHeaderAndTheConnectionSheet() {
        MockWebServer().use { server ->
            val vm = fixture(server)
            try {
                setApp(vm, server)
                waitText(chat.title)
                capture("fil")

                // Fil: the installation replaces the wordmark, with its live route.
                waitDescription("Connexion : Relais")
                compose.onNodeWithText("Cairn").assertDoesNotExist()
                assertTopsUnderStatusBar(compose.onNodeWithTag("installation-header"), statusBar)
                compose.onNodeWithContentDescription("Rechercher").assertIsDisplayed()
                compose.onNodeWithContentDescription("Compte et paramètres").assertIsDisplayed()
                compose
                    .onNodeWithContentDescription("Choisir une installation : Maison")
                    .performClick()
                waitText("Connexion")
                compose.onNodeWithText("Propriétaire").assertExists()
                compose.onNodeWithText("Réglages de l’installation").assertExists()
                compose.onNodeWithText("Agent · projet").assertDoesNotExist()
                capture("fil-connection-sheet")
                compose.onNodeWithContentDescription("Fermer Connexion").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Réglages de l’installation")
                        .fetchSemanticsNodes()
                        .isEmpty()
                }

                // Conversation: a single row right under the status bar.
                compose.onNodeWithText(chat.title).performClick()
                waitDescription("Fichiers · 4")
                capture("chat")
                val header = compose.onNodeWithTag("conversation-header")
                assertTopsUnderStatusBar(header, statusBar)
                val height = header.fetchSemanticsNode().boundsInRoot.height / density
                assertTrue("Conversation chrome is $height dp", height <= 60f)
                compose
                    .onNodeWithText("Agent principal · Maison", useUnmergedTree = true)
                    .assertExists()
                compose.onNodeWithContentDescription("Options de la conversation").performClick()
                compose.onNodeWithText("Connexion").performClick()
                waitText("Agent · projet")
                compose.onNodeWithText("Agent principal · Cairn").assertExists()
                compose.onNodeWithText("Relais").assertExists()
                capture("chat-connection-sheet")
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun switchingFromTheConnectionSheetSelectsTheOtherInstallation() {
        MockWebServer().use { server ->
            val vm = fixture(server)
            try {
                setApp(vm, server, dark = false)
                waitText(chat.title)
                compose
                    .onNodeWithContentDescription("Choisir une installation : Maison")
                    .performClick()
                waitText("Réglages de l’installation")
                val picker = hasContentDescription("Changer d’installation") and isEnabled()
                compose.waitUntil(30000) {
                    compose.onAllNodes(picker).fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNode(picker).performClick()
                compose.onNodeWithText("Atelier partagé").performClick()
                compose.waitUntil(30000) {
                    // Reading the tree lets Robolectric run the main looper.
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    vm.state.value.installation?.id == "shared"
                }
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
