package build.cairn.app.ui

import android.app.Application
import android.view.View
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.union
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.getOrNull
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
 * The top-level installation header, whose description names the installation then its state, e.g.
 * "Installation : Maison, En ligne · Relais".
 */
internal fun installationHeader(name: String = "", state: String = ""): SemanticsMatcher =
    hasContentDescription("Installation : $name", substring = true) and
        hasContentDescription(state, substring = true)

/** Issue #176: one header per screen, system insets applied once, and the Connexion sheet. */
abstract class UnifiedHeaderCases {
    protected open fun capture(name: String) {}

    /** Robolectric supplies the status bar and optional camera cutout here. */
    protected open fun applySystemInsets(view: View) {}

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
            provider = "claude",
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
        // Starting the server is network I/O, which Android forbids on the main thread.
        val origin = server.url("/").toString()
        compose.setContent {
            val local = LocalDensity.current
            val inset =
                WindowInsets.statusBars.union(WindowInsets.displayCutout).getTop(local).toFloat()
            val current = LocalView.current
            SideEffect {
                topInset = inset
                density = local.density
                view = current
            }
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                vm.connect(origin)
            }
            CairnTheme(if (dark) "dark" else "light") { CairnApp(vm = vm) }
        }
    }

    private var topInset = 0f
    private var density = 1f
    private var view: View? = null

    private fun waitText(value: String) =
        compose.waitUntil(30000) {
            compose.onAllNodesWithText(value).fetchSemanticsNodes().isNotEmpty()
        }

    private fun waitDescription(value: String) =
        compose.waitUntil(30000) {
            compose.onAllNodesWithContentDescription(value).fetchSemanticsNodes().isNotEmpty()
        }

    /**
     * The first content sits below both the status bar and camera cutout. Applying their union
     * twice, or not at all, moves it.
     */
    private fun assertTopsUnderSystemInsets(node: SemanticsNodeInteraction) {
        compose.runOnUiThread { applySystemInsets(checkNotNull(view)) }
        compose.waitForIdle()
        assertTrue("The top system inset is $topInset px", topInset > 0f)
        val top = node.fetchSemanticsNode().boundsInRoot.top
        assertTrue("Top $top should follow the $topInset px system inset", abs(top - topInset) < 2f)
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
                compose.waitUntil(30000) {
                    compose
                        .onAllNodes(installationHeader("Maison", "En ligne · Relais"))
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Cairn").assertDoesNotExist()
                assertTopsUnderSystemInsets(compose.onNodeWithTag("installation-header"))
                compose.onNodeWithContentDescription("Rechercher").assertIsDisplayed()
                compose.onNodeWithContentDescription("Compte et paramètres").assertIsDisplayed()
                compose
                    .onNode(installationHeader("Maison"))
                    .assert(
                        SemanticsMatcher("chooses an installation") {
                            it.config.getOrNull(SemanticsActions.OnClick)?.label ==
                                "Choisir une installation"
                        }
                    )
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
                compose
                    .onNodeWithContentDescription(
                        "Changer de conversation : ${chat.title} · Dernier harness : Claude Code"
                    )
                    .assertExists()
                capture("chat")
                val header = compose.onNodeWithTag("conversation-header")
                assertTopsUnderSystemInsets(header)
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
                // Keeps the original failure when no installation was opened yet.
                runCatching { vm.api.closeStreams() }
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
                compose.onNode(installationHeader("Maison")).performClick()
                waitText("Réglages de l’installation")
                val picker =
                    hasContentDescription("Changer d’installation : Maison") and isEnabled()
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
                // Keeps the original failure when no installation was opened yet.
                runCatching { vm.api.closeStreams() }
            }
        }
    }
}
