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
        /** The preview reports the delegation as suspended by an earlier change. */
        val alreadySuspended: AtomicBoolean = AtomicBoolean(false),
        /** The next agent update is rejected. */
        val rejectUpdate: AtomicBoolean = AtomicBoolean(false),
        /** Workspace reloads fail. */
        val failRefresh: AtomicBoolean = AtomicBoolean(false),
        /** The machine list fails to load. */
        val failNodes: AtomicBoolean = AtomicBoolean(false),
    )

    private fun fixture(): Fixture {
        val saved = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val agents = AtomicReference(listOf(main, release))
        val conflict = AtomicBoolean(false)
        val loseCreation = AtomicBoolean(false)
        val rejectToken = AtomicBoolean(false)
        val tokens = CopyOnWriteArrayList<String>()
        val alreadySuspended = AtomicBoolean(false)
        val rejectUpdate = AtomicBoolean(false)
        val failRefresh = AtomicBoolean(false)
        val failNodes = AtomicBoolean(false)
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
                        if (rejectUpdate.getAndSet(false))
                            return response
                                .setResponseCode(429)
                                .setBody("""{"error":"Trop de requêtes"}""")
                        // Like the server: a stale revision is refused, an absent one is not
                        // checked.
                        val sent = body["revision"]?.jsonPrimitive?.content.orEmpty()
                        val stored =
                            agents.get().find { it.id == path.removePrefix("/agents/") }?.revision
                        val stale = sent.isNotEmpty() && stored != null && sent != stored
                        if (conflict.getAndSet(false) || stale)
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
                    if (path == "/tasks" && failRefresh.get())
                        return response
                            .setResponseCode(409)
                            .setBody("""{"error":"Rechargement impossible"}""")
                    if (path == "/nodes" && failNodes.get())
                        return response
                            .setResponseCode(409)
                            .setBody("""{"error":"Nodes indisponibles"}""")
                    val body =
                        when {
                            path.endsWith("/preview") ->
                                if (path.startsWith("/agents/agent"))
                                    """{"runs":{"queued":0,"running":0},"delegations":[{"id":"bot","pluginId":"discord","name":"#support","enabled":true,"suspended":true,"blocked":null,"alreadySuspended":${alreadySuspended.get()}}]}"""
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
        return Fixture(
            server,
            saved,
            agents,
            conflict,
            loseCreation,
            rejectToken,
            tokens,
            alreadySuspended,
            rejectUpdate,
            failRefresh,
            failNodes,
        )
    }

    private var backDispatcher: OnBackPressedDispatcher? = null
    private val left = AtomicBoolean(false)

    /**
     * Shows the agent page against a fresh fixture, runs [body], then unmounts the page and clears
     * its view model before the server closes, so no work leaks into the next journey.
     */
    private fun journey(
        agentId: String,
        prepare: (Fixture) -> Unit = {},
        body: (Fixture) -> Unit,
    ) {
        val fixture = fixture().also(prepare)
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
            // Live grants are sent next to the access policy, never inside it.
            assertEquals(JsonArray(listOf(JsonPrimitive("prod"))), body["plugins"])
            assertFalse("plugins" in access)
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
            assertEquals(JsonArray(listOf(JsonPrimitive("gpu"))), body["exclusiveNodes"])
            assertFalse("exclusiveNodes" in access)
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
            assertEquals(JsonArray(listOf(JsonPrimitive("prod"))), body["plugins"])
            assertFalse("reapproveDelegations" in body)
        }
    }

    @Test
    fun aDelegationSuspendedEarlierIsNotReapprovedByDefault() {
        journey("agent") { fixture ->
            fixture.alreadySuspended.set(true)
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Release bot")
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
            }
            // Reapproving would also approve the earlier, unreviewed change: it stays unchecked.
            compose.onNodeWithText("Réapprouver les nouveaux accès pour « #support »").assertIsOff()
            compose
                .onNodeWithText(
                    "Déjà suspendu par une modification précédente. Ne le réapprouvez qu’après avoir vérifié ses accès actuels."
                )
                .assertExists()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            assertFalse("reapproveDelegations" in fixture.saved.single().second)
        }
    }

    @Test
    fun aReplayedCreationReportsFailedLatestEditsAndAFailedReload() {
        journey("") { fixture ->
            fixture.loseCreation.set(true)
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Nouvel agent")
            back()
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Sauvegarde de l’agent non confirmée", substring = true)

            // Edited meanwhile: the replay recovers the agent, then its update is rejected.
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Agent de release")
            back()
            fixture.rejectUpdate.set(true)
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText(
                "Agent enregistré, mais les dernières modifications n’ont pas été enregistrées. Réessayez.",
                substring = true,
            )
            assertFalse(left.get())

            // The update is saved, but the workspace reload fails: it is reported, not swallowed.
            fixture.failRefresh.set(true)
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Agent enregistré, mais le rechargement a échoué. Réessayez.")
            assertFalse(left.get())

            // Retrying only reloads.
            fixture.failRefresh.set(false)
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            compose.waitUntil(15000) { left.get() }
            assertEquals(
                listOf("/agents", "/agents", "/agents/created", "/agents/created"),
                fixture.saved.map { it.first },
            )
            assertEquals(
                listOf("Agent de release"),
                fixture.agents.get().filter { it.id == "created" }.map { it.name },
            )
        }
    }

    @Test
    fun nodeAccessCannotChangeUntilTheMachinesLoad() {
        journey("agent", prepare = { it.failNodes.set(true) }) { fixture ->
            open(AgentSection.Nodes)
            waitForText("Impossible de charger les nodes", substring = true)
            compose
                .onNodeWithContentDescription("Toutes les nodes, y compris futures")
                .assertIsNotEnabled()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur Runner actuel")
                .assertIsNotEnabled()

            fixture.failNodes.set(false)
            compose.onNodeWithText("Réessayer").performClick()
            waitForDescription("Exécuter ses conversations sur gpu-box")
            compose
                .onNodeWithText("Impossible de charger les nodes", substring = true)
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Toutes les nodes, y compris futures")
                .assertIsEnabled()
        }
    }

    @Test
    fun aHeldGrantOnAnUngrantablePluginIsShownAndCanBeRemoved() {
        // The Discord package gives agents nothing: an old grant on its bot is kept but useless.
        journey(
            "agent",
            prepare = { fixture ->
                fixture.agents.set(
                    fixture.agents.get().map {
                        if (it.id == "agent") it.copy(plugins = listOf("bot")) else it
                    }
                )
            },
        ) { fixture ->
            waitForText("⚠ 1 point à vérifier")
            open(AgentSection.Plugins)
            waitForDescription("Retirer #support")
            compose.onNodeWithText("Indisponible").assertExists()
            compose
                .onNodeWithText("Ce plugin ne donne plus d’accès aux agents", substring = true)
                .assertExists()
            compose.onNodeWithContentDescription("Retirer #support").performClick()
            compose.onNodeWithText("Indisponible").assertDoesNotExist()
            back()
            compose.onNodeWithText("⚠ 1 point à vérifier").assertDoesNotExist()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            assertEquals(JsonArray(emptyList()), fixture.saved.single().second["plugins"])
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
                    if (it.id == "agent")
                        it.copy(
                            description = "Changée ailleurs",
                            timeoutMinutes = 45,
                            revision = "r2",
                        )
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
            // The untouched time limit takes the newer value instead of restoring the old one.
            assertEquals(45, body["timeoutMinutes"]!!.jsonPrimitive.int)
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
    fun aRejectedTokenKeepsTheSavedRevisionAndARetrySavesOnlyTheToken() {
        journey("agent") { fixture ->
            fixture.rejectToken.set(true)
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Release bot")
            back()
            open(AgentSection.Github)
            compose.onNodeWithText(GithubMode.Dedicated.label).performClick()
            compose.onNodeWithText("Jeton GitHub dédié").performTextInput("fixture-token")
            back()

            // The agent is saved, its token is rejected.
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            waitForText("Agent enregistré, mais le jeton GitHub n’a pas été enregistré. Réessayez.")
            compose.onNodeWithText("● 1 modification non enregistrée").assertExists()

            // The retry neither resends the old revision nor loses the entered token.
            waitForEnabled("Enregistrer")
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) {
                fixture.tokens.size == 2 &&
                    compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
            }
            compose
                .onNodeWithText("Cet agent a été modifié ailleurs", substring = true)
                .assertDoesNotExist()
            assertEquals(listOf("/agents/agent"), fixture.saved.map { it.first })
            assertEquals("r1", fixture.saved.single().second["revision"]!!.jsonPrimitive.content)
            assertEquals(
                "Release bot",
                fixture.agents.get().single { it.id == "agent" }.name,
            )
        }
    }

    @Test
    fun creationRetriesKeepOneAgentAfterALostResponseAConflictAndARejectedToken() {
        journey("") { fixture ->
            fixture.loseCreation.set(true)
            fixture.rejectToken.set(true)
            // A new agent starts without access: the owner grants what it needs.
            compose.onNodeWithContentDescription("Projets, Aucun").assertExists()
            compose.onNodeWithContentDescription("GitHub, ${GithubMode.None.label}").assertExists()
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

            // The retry replays the same creation identity, then updates that agent from the
            // revision it was created with: a change made elsewhere meanwhile is a conflict.
            fixture.conflict.set(true)
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Cet agent a été modifié ailleurs", substring = true)
            assertEquals("c1", fixture.saved[2].second["revision"]!!.jsonPrimitive.content)

            // Reloading keeps the draft; a further edit is saved from the created revision and
            // the token is then rejected.
            compose.onNodeWithText("Recharger").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Recharger").fetchSemanticsNodes().isEmpty()
            }
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Agent de release")
            back()
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            waitForText("Agent enregistré, mais le jeton GitHub n’a pas été enregistré. Réessayez.")
            assertFalse(left.get())

            // The last retry saves only the token of that agent.
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            compose.waitUntil(15000) { left.get() }
            val requests = fixture.saved.map { it.first }
            assertEquals(
                listOf("/agents", "/agents", "/agents/created", "/agents/created"),
                requests,
            )
            assertEquals("c1", fixture.saved[3].second["revision"]!!.jsonPrimitive.content)
            val creation = fixture.saved[0].second
            assertEquals(creation["creationId"], fixture.saved[1].second["creationId"])
            assertNotNull(creation["creationId"])
            assertFalse("revision" in creation)
            val access = creation["access"]!!.jsonObject
            assertEquals(JsonArray(emptyList()), access["projects"])
            assertEquals(JsonArray(emptyList()), access["skills"])
            assertEquals(JsonArray(emptyList()), access["mcps"])
            assertEquals(JsonPrimitive(false), access["github"])
            assertEquals(JsonArray(listOf(JsonPrimitive(LOCAL_NODE_ID))), access["nodes"])
            assertEquals(
                "Agent de release",
                fixture.saved[3].second["name"]!!.jsonPrimitive.content,
            )
            assertEquals(
                listOf("Agent de release"),
                fixture.agents.get().filter { it.id == "created" }.map { it.name },
            )
            assertEquals(2, fixture.tokens.size)
        }
    }
}
