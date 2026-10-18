package build.cairn.app.ui

import android.app.Application
import androidx.activity.OnBackPressedDispatcher
import androidx.activity.compose.LocalOnBackPressedDispatcherOwner
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/**
 * The agent page: every permission is set on the agent, saved from one draft, reviewed when it adds
 * a power or a secret. Shared by the Robolectric suite and the device suite.
 */
abstract class AgentPageCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    private val main =
        Agent(
            MAIN_AGENT_ID,
            "Agent principal",
            revision = "m1",
            access = AccessPolicy(nodes = listOf(LOCAL_NODE_ID)),
        )
    private val release =
        Agent(
            "agent",
            "Release engineer",
            "Publie les versions web et Android",
            model = "gpt-5",
            revision = "r1",
            access =
                AccessPolicy(
                    nodes = listOf(LOCAL_NODE_ID),
                    projects = listOf("p1", "p2"),
                    skills = listOf("global/review"),
                    github = false,
                ),
        )

    private val nodes =
        """[{"id":"$LOCAL_NODE_ID","name":"local","local":true,"status":"online","accepting":true,"executionReady":true,"exclusiveReservations":true},""" +
            """{"id":"gpu","name":"gpu-box","status":"online","accepting":true,"executionReady":true,"exclusiveReservations":true,"tags":["gpu","linux"]},""" +
            """{"id":"mac","name":"mac-mini","status":"offline","accepting":true,"tags":["macos"]}]"""

    private val catalog =
        PluginCatalog(
            packages =
                listOf(
                    PluginEntry(
                        "onepassword",
                        builtin = true,
                        manifest =
                            PluginManifest(
                                id = "onepassword",
                                name = "1Password",
                                agentAccess = PluginAgentAccess("Read-only secrets"),
                            ),
                    ),
                    PluginEntry(
                        "discord",
                        builtin = true,
                        manifest =
                            PluginManifest(
                                id = "discord",
                                name = "Discord",
                                capabilities = listOf("conversations.submit"),
                            ),
                    ),
                ),
            connections =
                listOf(
                    PluginConnection("prod", "onepassword", "Production"),
                    PluginConnection("staging", "onepassword", "Staging", enabled = false),
                    PluginConnection("bot", "discord", "#support", agentId = "agent"),
                ),
        )

    private class Fixture(
        val server: MockWebServer,
        val saved: CopyOnWriteArrayList<Pair<String, JsonObject>>,
        val agents: AtomicReference<List<Agent>>,
        val conflict: AtomicBoolean,
        val loseCreation: AtomicBoolean,
        val rejectToken: AtomicBoolean,
        val tokens: CopyOnWriteArrayList<String>,
    )

    private fun fixture(): Fixture {
        val saved = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val agents = AtomicReference(listOf(main, release))
        val conflict = AtomicBoolean(false)
        val loseCreation = AtomicBoolean(false)
        val rejectToken = AtomicBoolean(false)
        val tokens = CopyOnWriteArrayList<String>()
        val server = MockWebServer()
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path =
                        request.path!!
                            .substringBefore('?')
                            .removePrefix("/api/installations/fixture/api")
                    val text = request.body.readUtf8()
                    val response = MockResponse().setHeader("Content-Type", "application/json")
                    if (request.method == "POST" && path == "/agents") {
                        val body = wireJson.parseToJsonElement(text).jsonObject
                        saved += path to body
                        // A replayed creation identity returns the agent it already created.
                        val created =
                            agents.get().find { it.id == "created" }
                                ?: wireJson
                                    .decodeFromJsonElement<Agent>(body)
                                    .copy(id = "created", revision = "c1")
                                    .also { agents.set(agents.get() + it) }
                        if (loseCreation.getAndSet(false))
                            return MockResponse()
                                .setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                        return response.setBody(wireJson.encodeToString(created))
                    }
                    if (request.method == "PUT" && path.endsWith("/github-token")) {
                        tokens += path
                        if (rejectToken.getAndSet(false))
                            return response
                                .setResponseCode(400)
                                .setBody("""{"error":"Jeton refusé"}""")
                        return response.setBody("""{"configured":true}""")
                    }
                    if (
                        request.method == "PUT" &&
                            path.startsWith("/agents/") &&
                            !path.endsWith("/github-token")
                    ) {
                        val body = wireJson.parseToJsonElement(text).jsonObject
                        saved += path to body
                        if (conflict.getAndSet(false))
                            return response
                                .setResponseCode(409)
                                .setBody(
                                    """{"error":"This agent was changed elsewhere. Reload it before saving."}"""
                                )
                        val updated =
                            wireJson
                                .decodeFromJsonElement<Agent>(body)
                                .copy(
                                    id = path.removePrefix("/agents/"),
                                    revision = "saved-${saved.size}",
                                )
                        agents.set(agents.get().map { if (it.id == updated.id) updated else it })
                        return response.setBody(wireJson.encodeToString(updated))
                    }
                    val body =
                        when {
                            path.endsWith("/preview") ->
                                if (path.startsWith("/agents/agent"))
                                    """{"runs":{"queued":0,"running":0},"delegations":[{"id":"bot","pluginId":"discord","name":"#support","enabled":true,"suspended":true,"blocked":null}]}"""
                                else """{"runs":{"queued":0,"running":0},"delegations":[]}"""
                            path.endsWith("/github-token") -> """{"configured":false}"""
                            path == "/agents" -> wireJson.encodeToString(agents.get())
                            path == "/projects" ->
                                """[{"id":"p1","name":"cairn","path":"/srv/cairn"},{"id":"p2","name":"SheetOM","path":"/srv/sheetom"}]"""
                            path == "/skills" ->
                                """[{"name":"review","scope":"global"},{"name":"lint","scope":"p1"}]"""
                            path == "/nodes" -> nodes
                            path == "/plugins" -> wireJson.encodeToString(catalog)
                            path == "/api/installations" -> beaconInstallationsFixture()
                            path == "/api/account/session" -> beaconAccountFixture("fixture")
                            path.endsWith("/models") -> """{"models":[]}"""
                            path.endsWith("/overview") -> "{}"
                            else -> "[]"
                        }
                    return response.setBody(body)
                }
            }
        server.start()
        return Fixture(server, saved, agents, conflict, loseCreation, rejectToken, tokens)
    }

    private var backDispatcher: OnBackPressedDispatcher? = null
    private val left = AtomicBoolean(false)

    /**
     * Shows the agent page against a fresh fixture, runs [body], then unmounts the page and clears
     * its view model before the server closes, so no work leaks into the next journey.
     */
    private fun journey(agentId: String, body: (Fixture) -> Unit) {
        val fixture = fixture()
        fixture.server.use {
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var mounted by mutableStateOf(true)
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                backDispatcher = LocalOnBackPressedDispatcherOwner.current?.onBackPressedDispatcher
                var loaded by remember { mutableStateOf(false) }
                if (state.agents.isNotEmpty()) loaded = true
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(fixture.server.url("/").toString())
                }
                CairnTheme {
                    Surface(Modifier.fillMaxSize()) {
                        Box(Modifier.systemBarsPadding()) {
                            if (mounted && loaded)
                                AgentScreen(
                                    vm,
                                    state,
                                    agentId,
                                    back = { left.set(true) },
                                    chat = {},
                                    navigate = {},
                                )
                        }
                    }
                }
            }
            try {
                compose.waitUntil(30000) {
                    compose
                        .onAllNodesWithTag("agent-section-Nodes")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                body(fixture)
            } finally {
                compose.runOnIdle { mounted = false }
                compose.waitForIdle()
                compose.runOnIdle { models.clear() }
            }
        }
    }

    private fun open(section: AgentSection) {
        compose.onNodeWithTag("agent-section-${section.name}").performScrollTo().performClick()
        compose.waitForIdle()
    }

    private fun back() {
        compose.onNodeWithContentDescription("Retour").performClick()
        compose.waitForIdle()
    }

    /** Sections load their nodes and plugin instances when the page opens. */
    private fun waitForDescription(description: String) {
        compose.waitUntil(15000) {
            compose.onAllNodesWithContentDescription(description).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun waitForText(text: String, substring: Boolean = false) {
        compose.waitUntil(15000) {
            compose
                .onAllNodesWithText(text, substring = substring)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    /** A lost response starts connection recovery, which disables saving until it ends. */
    private fun waitForEnabled(text: String) {
        compose.waitUntil(20000) {
            compose.onAllNodes(hasText(text) and isEnabled()).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun confirmReview() {
        compose.waitUntil(15000) {
            compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
        }
        compose
            .onNode(hasText("Enregistrer") and hasAnyAncestor(hasTestTag("agent-review")))
            .performClick()
    }

    @Test
    fun mainAgentShowsFixedAccessAndKeepsItsEditableGrants() {
        journey(MAIN_AGENT_ID) { fixture ->
            compose
                .onNodeWithText("L’agent principal a toujours accès", substring = true)
                .assertExists()
            assertEquals(4, compose.onAllNodesWithText("🔒 Fixé").fetchSemanticsNodes().size)
            // Fixed rows do not open: Android used to offer restrictions the server rejects.
            compose.onNodeWithTag("agent-section-Projects").assertHasNoClickAction()
            compose.onNodeWithText("Aucun accès").assertExists()
            capture("agent-main")

            open(AgentSection.Plugins)
            waitForDescription("Accès à Production")
            compose.onNodeWithText("1Password · comptes de service").assertExists()
            compose.onNodeWithContentDescription("Accès à Production").assertIsOff().performClick()
            back()
            compose.onNodeWithText("● 1 modification non enregistrée").assertExists()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            val (path, body) = fixture.saved.single()
            assertEquals("/agents/$MAIN_AGENT_ID", path)
            val access = body["access"]!!.jsonObject
            assertEquals(JsonArray(listOf(JsonPrimitive("prod"))), access["plugins"])
            assertEquals(JsonNull, access["projects"])
            assertEquals("m1", body["revision"]!!.jsonPrimitive.content)
            assertFalse("reapproveDelegations" in body)
        }
    }

    @Test
    fun exclusiveReservationIsGrantedOnlyWhereTheAgentRuns() {
        journey("agent") { fixture ->
            open(AgentSection.Nodes)
            waitForDescription("Réserver gpu-box en exclusivité")
            compose
                .onNodeWithContentDescription("Réserver gpu-box en exclusivité")
                .performScrollTo()
                .assertIsNotEnabled()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur gpu-box")
                .performScrollTo()
                .assertIsOff()
                .performClick()
            compose
                .onNodeWithContentDescription("Réserver gpu-box en exclusivité")
                .assertIsEnabled()
                .performClick()
                .assertIsOn()
            // A machine whose runtime cannot reserve stays unavailable even when it runs here.
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur mac-mini")
                .performScrollTo()
                .performClick()
            compose
                .onNodeWithContentDescription("Réserver mac-mini en exclusivité")
                .assertIsNotEnabled()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur mac-mini")
                .performClick()
            compose
                .onNodeWithContentDescription("Réserver gpu-box en exclusivité")
                .performScrollTo()
            capture("agent-nodes")
            back()
            waitForText("2 sur 3 · exclusivité sur 1")
            capture("agent-overview")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText("Ajout : réservation exclusive sur gpu-box", substring = true)
                .assertExists()
            compose.onNodeWithText("Réapprouver les nouveaux accès pour « #support »").assertIsOn()
            capture("agent-review")
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            val body = fixture.saved.single().second
            val access = body["access"]!!.jsonObject
            assertEquals(
                listOf(LOCAL_NODE_ID, "gpu"),
                access["nodes"]!!.jsonArray.map { it.jsonPrimitive.content },
            )
            assertEquals(JsonArray(listOf(JsonPrimitive("gpu"))), access["exclusiveNodes"])
            assertEquals(JsonArray(listOf(JsonPrimitive("bot"))), body["reapproveDelegations"])
            assertEquals("r1", body["revision"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun onePasswordAccountIsGrantedFromTheAgentWithReadableSkills() {
        journey("agent") { fixture ->
            open(AgentSection.Skills)
            compose.onNodeWithText("review · Global").assertExists()
            compose.onNodeWithText("lint · cairn").assertExists()
            compose.onNodeWithText("global/review").assertDoesNotExist()
            back()

            open(AgentSection.Plugins)
            waitForDescription("Accès à Production")
            compose.onNodeWithText("1Password · comptes de service").assertExists()
            compose
                .onNodeWithText("Lecture seule · aucun accès par défaut, Main compris")
                .assertExists()
            compose.onNodeWithText("Compte désactivé").assertExists()
            compose
                .onNodeWithText("Répond en tant que cet agent sur « #support »", substring = true)
                .performScrollTo()
                .assertExists()
            compose
                .onNodeWithContentDescription("Accès à Production")
                .performScrollTo()
                .performClick()
            compose.onNodeWithContentDescription("Accès à Production").performScrollTo()
            capture("agent-secrets")
            back()
            compose.onNodeWithText("1Password : 1 compte").assertExists()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText(
                    "Ajout : 1Password « Production », effectif immédiatement, y compris dans les conversations en cours."
                )
                .assertExists()
            // Declining the reapproval is possible: the bot then stops until it is reapproved.
            compose
                .onNodeWithText("Réapprouver les nouveaux accès pour « #support »")
                .performClick()
            compose
                .onNodeWithText("Ce bot sera suspendu jusqu’à sa reconfirmation sur l’instance.")
                .assertExists()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            val body = fixture.saved.single().second
            assertEquals(
                JsonArray(listOf(JsonPrimitive("prod"))),
                body["access"]!!.jsonObject["plugins"],
            )
            assertFalse("reapproveDelegations" in body)
        }
    }

    @Test
    fun unsavedChangesAskBeforeLeavingAndAConflictKeepsTheDraft() {
        journey("agent") { fixture ->
            // Without changes, back leaves at once.
            compose.runOnIdle { checkNotNull(backDispatcher).onBackPressed() }
            compose.waitUntil(5000) { left.get() }
            left.set(false)

            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Release bot")
            compose.runOnIdle { checkNotNull(backDispatcher).onBackPressed() }
            compose.onNodeWithTag("agent-section-Nodes").assertExists()
            compose.runOnIdle { checkNotNull(backDispatcher).onBackPressed() }
            waitForText("Quitter sans enregistrer ?")
            compose.onNodeWithText("La modification non enregistrée sera perdue.").assertExists()
            compose.onNodeWithText("Continuer").performClick()
            assertFalse(left.get())
            compose.onNodeWithText("● 1 modification non enregistrée").assertExists()

            // Someone else saved meanwhile: the draft stays and can be reapplied on top.
            fixture.conflict.set(true)
            fixture.agents.set(
                fixture.agents.get().map {
                    if (it.id == "agent") it.copy(description = "Changée ailleurs", revision = "r2")
                    else it
                }
            )
            compose.onNodeWithText("Enregistrer").performClick()
            // The preview reports the delegation answering as this agent.
            confirmReview()
            waitForText("Cet agent a été modifié ailleurs", substring = true)
            compose.onNodeWithText("● 1 modification non enregistrée").assertExists()
            compose.onNodeWithText("Recharger").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Recharger").fetchSemanticsNodes().isEmpty() &&
                    compose
                        .onAllNodes(hasText("Enregistrer") and isEnabled())
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) {
                fixture.saved.size == 2 &&
                    compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
            }
            val body = fixture.saved.last().second
            assertEquals("Release bot", body["name"]!!.jsonPrimitive.content)
            assertEquals("Changée ailleurs", body["description"]!!.jsonPrimitive.content)
            assertEquals("r2", body["revision"]!!.jsonPrimitive.content)

            // Discarding restores the saved agent.
            open(AgentSection.Advanced)
            compose.onNodeWithText("Sans limite de temps").performClick()
            back()
            compose.onNodeWithText("Annuler").performClick()
            compose.onNodeWithTag("agent-save-bar").assertDoesNotExist()
        }
    }

    @Test
    fun creationRetriesKeepOneAgentAfterALostResponseAndARejectedToken() {
        journey("") { fixture ->
            fixture.loseCreation.set(true)
            fixture.rejectToken.set(true)
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Nouvel agent")
            back()
            open(AgentSection.Github)
            compose.onNodeWithText(GithubMode.Dedicated.label).performClick()
            compose.onNodeWithText("Jeton GitHub dédié").performTextInput("fixture-token")
            back()

            // The response is lost after the server created the agent.
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Sauvegarde de l’agent non confirmée", substring = true)
            capture("agent-creation-uncertain")

            // The retry replays the same creation identity; the token is then rejected.
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Agent enregistré, mais le jeton GitHub n’a pas été enregistré. Réessayez.")
            assertFalse(left.get())

            // The last retry updates that agent and saves only its token.
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            compose.waitUntil(15000) { left.get() }
            val requests = fixture.saved.map { it.first }
            assertEquals(
                listOf("/agents", "/agents", "/agents/created", "/agents/created"),
                requests,
            )
            assertEquals(
                fixture.saved[0].second["creationId"],
                fixture.saved[1].second["creationId"],
            )
            assertNotNull(fixture.saved[0].second["creationId"])
            assertEquals(1, fixture.agents.get().count { it.name == "Nouvel agent" })
            assertEquals(2, fixture.tokens.size)
        }
    }
}
