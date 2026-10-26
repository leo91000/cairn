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
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            createdAt = 0,
            id = MAIN_AGENT_ID,
            name = "Agent principal",
            revision = "m1",
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    projects = null,
                    skills = null,
                    mcps = null,
                    nodes = listOf(LOCAL_NODE_ID),
                ),
        )
    private val release =
        Agent(
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            createdAt = 0,
            id = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
            name = "Release engineer",
            description = "Publie les versions web et Android",
            model = "gpt-5",
            revision = "r1",
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    mcps = null,
                    nodes = listOf(LOCAL_NODE_ID),
                    projects =
                        listOf(
                            "2450c72e-6174-56da-b0fc-b06868e8e8e5",
                            "6501040b-e89b-5102-9fa7-24510fb014c8",
                        ),
                    skills = listOf("global/review"),
                ),
        )

    private val nodes =
        """[{"id":"$LOCAL_NODE_ID","name":"local","local":true,"status":"online","accepting":true,"executionReady":true,"exclusiveReservations":true},""" +
            """{"id":"gpu","name":"gpu-box","status":"online","accepting":true,"executionReady":true,"exclusiveReservations":true,"tags":["gpu","linux"]},""" +
            """{"id":"mac","name":"mac-mini","status":"offline","accepting":true,"tags":["macos"]},""" +
            """{"id":"desk","name":"desk-native","status":"online","accepting":true,"executionReady":true,"capabilities":{"backend":"native"}}]"""

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
                    // GitHub accounts are plugin instances granted like any other.
                    PluginEntry(
                        "github",
                        builtin = true,
                        manifest =
                            PluginManifest(
                                id = "github",
                                name = "GitHub",
                                agentAccess =
                                    PluginAgentAccess(
                                        "Granted agents' runs push and use the GitHub CLI as this account."
                                    ),
                                instances = PluginInstances("Account", "Accounts"),
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
                    PluginConnection(
                        "bot",
                        "discord",
                        "#support",
                        agentId = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
                    ),
                    PluginConnection("gh-work", "github", "octo-work"),
                    PluginConnection("gh-home", "github", "octo-home"),
                ),
        )

    /** The server's refusal of two accounts of a package whose runs use their credentials. */
    private val oneInstance =
        "Grant one GitHub instance per agent: its runs use that instance's credentials."

    private class Fixture(
        val server: MockWebServer,
        val saved: CopyOnWriteArrayList<Pair<String, JsonObject>>,
        val agents: AtomicReference<List<Agent>>,
        val conflict: AtomicBoolean,
        val loseCreation: AtomicBoolean,
        /** The preview reports the delegation as suspended by an earlier change. */
        val alreadySuspended: AtomicBoolean = AtomicBoolean(false),
        /** The next agent update is rejected. */
        val rejectUpdate: AtomicBoolean = AtomicBoolean(false),
        /** Workspace reloads fail. */
        val failRefresh: AtomicBoolean = AtomicBoolean(false),
        /** The machine list fails to load. */
        val failNodes: AtomicBoolean = AtomicBoolean(false),
        /** The plugin catalog fails to load. */
        val failPlugins: AtomicBoolean = AtomicBoolean(false),
        /** The installation's MCP connections, as `GET /mcps` returns them. */
        val mcps: AtomicReference<String> = AtomicReference("[]"),
    )

    private fun fixture(): Fixture {
        val saved = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val agents = AtomicReference(listOf(main, release))
        val conflict = AtomicBoolean(false)
        val loseCreation = AtomicBoolean(false)
        val alreadySuspended = AtomicBoolean(false)
        val rejectUpdate = AtomicBoolean(false)
        val failRefresh = AtomicBoolean(false)
        val failNodes = AtomicBoolean(false)
        val failPlugins = AtomicBoolean(false)
        val mcps = AtomicReference("[]")
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
                            agents.get().find { it.id == "625c462a-fcff-5985-b84a-e212b3ac665d" }
                                ?: wireJson
                                    .decodeFromJsonElement<Agent>(
                                        JsonObject(
                                            body["settings"]!!.jsonObject +
                                                mapOf(
                                                    "id" to
                                                        JsonPrimitive(
                                                            "ecf23b20-8841-5456-a8d8-1e43edc19cb9"
                                                        ),
                                                    "revision" to JsonPrimitive("c1"),
                                                    "createdAt" to JsonPrimitive(0),
                                                )
                                        )
                                    )
                                    .copy(
                                        id = "625c462a-fcff-5985-b84a-e212b3ac665d",
                                        revision = "c1",
                                    )
                                    .also { agents.set(agents.get() + it) }
                        if (loseCreation.getAndSet(false))
                            return MockResponse()
                                .setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                        return response.setBody(wireJson.encodeToString(created))
                    }
                    if (request.method == "PUT" && path.startsWith("/agents/")) {
                        val body = wireJson.parseToJsonElement(text).jsonObject
                        saved += path to body
                        // Like the server: runs use one account's credentials per package.
                        val github =
                            body["settings"]!!.jsonObject["plugins"]?.jsonArray.orEmpty().count {
                                it.jsonPrimitive.content.startsWith("gh-")
                            }
                        if (github > 1)
                            return response
                                .setResponseCode(400)
                                .setBody("""{"error":"$oneInstance"}""")
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
                                .decodeFromJsonElement<Agent>(
                                    JsonObject(
                                        body["settings"]!!.jsonObject +
                                            mapOf(
                                                "id" to
                                                    JsonPrimitive(path.removePrefix("/agents/")),
                                                "revision" to JsonPrimitive("saved"),
                                                "createdAt" to JsonPrimitive(0),
                                            )
                                    )
                                )
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
                    if (path == "/plugins" && failPlugins.get())
                        return response
                            .setResponseCode(409)
                            .setBody("""{"error":"Plugins indisponibles"}""")
                    if (path == "/nodes" && failNodes.get())
                        return response
                            .setResponseCode(409)
                            .setBody("""{"error":"Nodes indisponibles"}""")
                    val body =
                        when {
                            path.endsWith("/preview") ->
                                if (path.startsWith("/agents/d3153f9f-724b-59c3-9614-ab78f231c4d3"))
                                    """{"runs":{"queued":0,"running":0},"delegations":[{"id":"bot","pluginId":"discord","name":"#support","enabled":true,"suspended":true,"blocked":null,"alreadySuspended":${alreadySuspended.get()}}]}"""
                                else """{"runs":{"queued":0,"running":0},"delegations":[]}"""
                            path == "/agents" ->
                                wireJson.encodeToString(
                                    build.cairn.contracts.ListAgentsOutput(agents.get())
                                )
                            path == "/projects" ->
                                """{"projects":[{"id":"2450c72e-6174-56da-b0fc-b06868e8e8e5","name":"cairn","path":"/srv/cairn","description":"","baseBranch":"main","sourceMode":"remote","origin":"","source":null,"revision":"fixture-revision","createdAt":0},{"id":"6501040b-e89b-5102-9fa7-24510fb014c8","name":"SheetOM","path":"/srv/sheetom","description":"","baseBranch":"main","sourceMode":"remote","origin":"","source":null,"revision":"fixture-revision","createdAt":0}]}"""
                            path == "/mcps" -> """{"mcps":${mcps.get()}}"""
                            path == "/skills" ->
                                """[{"name":"review","scope":"global"},{"name":"lint","scope":"2450c72e-6174-56da-b0fc-b06868e8e8e5"}]"""
                            path == "/nodes" -> nodes
                            path == "/plugins" -> wireJson.encodeToString(catalog)
                            path == "/api/installations" -> beaconInstallationsFixture()
                            path == "/api/account/session" -> beaconAccountFixture("fixture")
                            path.endsWith("/models") -> """{"models":[]}"""
                            path.endsWith("/overview") -> "{}"
                            else ->
                                if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/projects") ==
                                        true
                                )
                                    """{"projects":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/agents") == true
                                )
                                    """{"agents":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/tasks") == true
                                )
                                    """{"tasks":[]}"""
                                else if (
                                    request.requestUrl?.encodedPath?.endsWith("/api/mcps") == true
                                )
                                    """{"mcps":[]}"""
                                else "[]"
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
            alreadySuspended,
            rejectUpdate,
            failRefresh,
            failNodes,
            failPlugins,
            mcps,
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
            assertEquals(3, compose.onAllNodesWithText("🔒 Fixé").fetchSemanticsNodes().size)
            // GitHub has no section of its own: its accounts are plugin grants.
            compose.onNodeWithTag("agent-section-Github").assertDoesNotExist()
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
            val access = body["settings"]!!.jsonObject["access"]!!.jsonObject
            // Live grants are sent next to the access policy, never inside it.
            assertEquals(
                JsonArray(listOf(JsonPrimitive("prod"))),
                body["settings"]!!.jsonObject["plugins"],
            )
            assertFalse("plugins" in access)
            assertEquals(JsonNull, access["projects"])
            assertEquals("m1", body["revision"]!!.jsonPrimitive.content)
            assertTrue(body["review"]!!.jsonObject["reapproveDelegations"]!!.jsonArray.isEmpty())
        }
    }

    @Test
    fun exclusiveReservationIsGrantedOnlyWhereTheAgentRuns() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
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
            val access = body["settings"]!!.jsonObject["access"]!!.jsonObject
            assertEquals(
                listOf(LOCAL_NODE_ID, "gpu"),
                access["nodes"]!!.jsonArray.map { it.jsonPrimitive.content },
            )
            assertEquals(
                JsonArray(listOf(JsonPrimitive("gpu"))),
                body["settings"]!!.jsonObject["exclusiveNodes"],
            )
            assertFalse("exclusiveNodes" in access)
            assertEquals(
                JsonArray(listOf(JsonPrimitive("bot"))),
                body["review"]!!.jsonObject["reapproveDelegations"],
            )
            assertEquals("r1", body["revision"]!!.jsonPrimitive.content)
        }
    }

    @Test
    fun onePasswordAccountIsGrantedFromTheAgentWithReadableSkills() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
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
                body["settings"]!!.jsonObject["plugins"],
            )
            assertTrue(body["review"]!!.jsonObject["reapproveDelegations"]!!.jsonArray.isEmpty())
        }
    }

    @Test
    fun aDelegationSuspendedEarlierIsNotReapprovedByDefault() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
            fixture.alreadySuspended.set(true)
            // An earlier suspension alone does not open the review.
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Release bot")
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                fixture.saved.size == 1 &&
                    compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
            }
            compose.onNodeWithTag("agent-review").assertDoesNotExist()
            assertTrue(
                fixture.saved
                    .single()
                    .second["review"]!!
                    .jsonObject["reapproveDelegations"]!!
                    .jsonArray
                    .isEmpty()
            )

            // When a grant opens it, the delegation is listed unchecked.
            open(AgentSection.Plugins)
            waitForDescription("Accès à Production")
            compose.onNodeWithContentDescription("Accès à Production").performClick()
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
            compose.waitUntil(15000) { fixture.saved.size == 2 }
            assertTrue(
                fixture.saved
                    .last()
                    .second["review"]!!
                    .jsonObject["reapproveDelegations"]!!
                    .jsonArray
                    .isEmpty()
            )
        }
    }

    @Test
    fun aGithubAccountIsAPluginGrantAndTwoAccountsShowTheServerRefusal() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
            open(AgentSection.Plugins)
            waitForDescription("Accès à octo-work")
            compose
                .onNodeWithText("Granted agents' runs push", substring = true)
                .performScrollTo()
                .assertExists()
            compose
                .onNodeWithContentDescription("Accès à octo-work")
                .performScrollTo()
                .performClick()
            compose
                .onNodeWithContentDescription("Accès à octo-home")
                .performScrollTo()
                .performClick()
            capture("agent-github-accounts")
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            // The server refuses two accounts whose runs would both receive credentials.
            waitForText(oneInstance, substring = true)
            assertEquals(
                listOf("gh-work", "gh-home"),
                fixture.saved.single().second["settings"]!!.jsonObject["plugins"]!!.jsonArray.map {
                    it.jsonPrimitive.content
                },
            )

            // One account saves.
            open(AgentSection.Plugins)
            compose
                .onNodeWithContentDescription("Accès à octo-home")
                .performScrollTo()
                .performClick()
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) {
                fixture.saved.size == 2 &&
                    compose.onAllNodesWithTag("agent-save-bar").fetchSemanticsNodes().isEmpty()
            }
            val body = fixture.saved.last().second
            assertEquals(
                JsonArray(listOf(JsonPrimitive("gh-work"))),
                body["settings"]!!.jsonObject["plugins"],
            )
            assertFalse("github" in body["settings"]!!.jsonObject["access"]!!.jsonObject)
        }
    }

    @Test
    fun switchingToNativeExecutionStartsFromNoNodesAndIsReviewed() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
            open(AgentSection.Nodes)
            waitForDescription("Exécuter ses conversations sur gpu-box")
            // A VM agent only sees VM machines.
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur desk-native")
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Exécution native sur l’hôte")
                .assertIsOff()
                .performClick()
            compose
                .onNodeWithText("Partage le GPU et les outils de l’hôte", substring = true)
                .assertExists()
            compose
                .onNodeWithContentDescription("Toutes les nodes VM, y compris futures")
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur Runner actuel")
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur gpu-box")
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur desk-native")
                .assertIsOff()
                .performClick()
            back()
            compose
                .onNodeWithContentDescription("Nodes, Exécution native · 1 sur 1, modifié")
                .assertExists()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText("Ajout : exécution native sur l’hôte", substring = true)
                .assertExists()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            val access =
                fixture.saved.single().second["settings"]!!.jsonObject["access"]!!.jsonObject
            assertEquals(JsonPrimitive("native"), access["backend"])
            assertEquals(JsonArray(listOf(JsonPrimitive("desk"))), access["nodes"])
        }
    }

    @Test
    fun nativeAgentCanUseAllCurrentAndFutureNodes() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
            open(AgentSection.Nodes)
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(
                        hasContentDescription("Exécution native sur l’hôte") and isEnabled()
                    )
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Exécution native sur l’hôte").performClick()
            waitForDescription("Toutes les nodes natives, y compris futures")
            compose
                .onNodeWithContentDescription("Toutes les nodes natives, y compris futures")
                .assertIsOff()
                .performClick()
                .assertIsOn()
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithTag("agent-review").fetchSemanticsNodes().isNotEmpty()
            }
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            val access =
                fixture.saved.single().second["settings"]!!.jsonObject["access"]!!.jsonObject
            assertEquals(JsonPrimitive("native"), access["backend"])
            assertEquals(JsonNull, access["nodes"])
        }
    }

    @Test
    fun everyMcpConnectionCanBeChosenEvenBeforeAnyExists() {
        journey(
            "d3153f9f-724b-59c3-9614-ab78f231c4d3",
            prepare = { fixture ->
                fixture.agents.set(
                    fixture.agents.get().map {
                        if (it.id == "d3153f9f-724b-59c3-9614-ab78f231c4d3")
                            it.copy(access = it.access.copy(mcps = emptyList()))
                        else it
                    }
                )
            },
        ) { fixture ->
            open(AgentSection.Mcp)
            compose.onNodeWithText("Aucune connexion MCP").assertExists()
            compose.onNodeWithText("Toutes, y compris futures").performClick()
            compose
                .onNodeWithText("Les nouvelles connexions MCP seront accessibles dès leur ajout.")
                .assertExists()
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            assertEquals(
                JsonNull,
                fixture.saved
                    .single()
                    .second["settings"]!!
                    .jsonObject["access"]!!
                    .jsonObject["mcps"],
            )
        }
    }

    @Test
    fun untickingAConnectionKeepsItsToolRestrictionForWhenItIsGrantedAgain() {
        journey(
            "d3153f9f-724b-59c3-9614-ab78f231c4d3",
            prepare = { fixture ->
                fixture.mcps.set(
                    """[{"id":"linear","name":"Linear","url":"https://mcp.linear.invalid","tools":[{"name":"read"},{"name":"write"}]}]"""
                )
                fixture.agents.set(
                    fixture.agents.get().map {
                        if (it.id == "d3153f9f-724b-59c3-9614-ab78f231c4d3")
                            it.copy(
                                access =
                                    it.access.copy(
                                        mcps = listOf("linear"),
                                        mcpTools =
                                            build.cairn.contracts.McpTools(
                                                mapOf("linear" to listOf("read"))
                                            ),
                                    )
                            )
                        else it
                    }
                )
            },
        ) { fixture ->
            open(AgentSection.Mcp)
            waitForText("Linear")
            compose.onNodeWithText("Linear").performClick()
            compose.onNodeWithText("● 1 modification non enregistrée").assertExists()
            // Granted again, it comes back with its restriction: nothing left to save.
            compose.onNodeWithText("Linear").performClick()
            compose.onNodeWithTag("agent-save-bar").assertDoesNotExist()
            compose.onNodeWithText("Outils : 2 activés sur la connexion · 1 choisis").assertExists()
        }
    }

    @Test
    fun aFailedReReadOnReloadMakesPluginsUnknown() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Release bot")
            back()
            fixture.conflict.set(true)
            fixture.failPlugins.set(true)
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            waitForText("Cet agent a été modifié ailleurs", substring = true)
            compose.onNodeWithText("Recharger").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Recharger").fetchSemanticsNodes().isEmpty()
            }
            // Known before, unknown now: never shown as it was.
            open(AgentSection.Plugins)
            waitForText("Impossible de charger les plugins", substring = true)
            compose.onNodeWithContentDescription("Accès à Production").assertDoesNotExist()
        }
    }

    @Test
    fun pluginGrantsStayAsTheyAreWhenTheCatalogFailsToLoad() {
        journey(
            "d3153f9f-724b-59c3-9614-ab78f231c4d3",
            prepare = { fixture ->
                fixture.failPlugins.set(true)
                fixture.agents.set(
                    fixture.agents.get().map {
                        if (it.id == "d3153f9f-724b-59c3-9614-ab78f231c4d3")
                            it.copy(plugins = listOf("prod"))
                        else it
                    }
                )
            },
        ) { fixture ->
            // Nothing is reported missing while the catalog is unknown.
            compose.onNodeWithText("point à vérifier", substring = true).assertDoesNotExist()
            open(AgentSection.Plugins)
            waitForText("Impossible de charger les plugins", substring = true)
            compose.onNodeWithText("1 instance accordée :").assertExists()
            compose.onNodeWithText("prod").assertExists()
            compose.onNodeWithText("Retirer").assertDoesNotExist()
            compose.onNodeWithText("Instance supprimée").assertDoesNotExist()

            fixture.failPlugins.set(false)
            compose.onNodeWithText("Réessayer").performClick()
            waitForDescription("Accès à Production")
            compose.onNodeWithContentDescription("Accès à Production").assertIsOn()
            compose
                .onNodeWithText("Impossible de charger les plugins", substring = true)
                .assertDoesNotExist()
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
                listOf(
                    "/agents",
                    "/agents",
                    "/agents/625c462a-fcff-5985-b84a-e212b3ac665d",
                    "/agents/625c462a-fcff-5985-b84a-e212b3ac665d",
                ),
                fixture.saved.map { it.first },
            )
            assertEquals(
                listOf("Agent de release"),
                fixture.agents
                    .get()
                    .filter { it.id == "625c462a-fcff-5985-b84a-e212b3ac665d" }
                    .map { it.name },
            )
        }
    }

    @Test
    fun nodeAccessCannotChangeUntilTheMachinesLoad() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3", prepare = { it.failNodes.set(true) }) {
            fixture ->
            // Summaries computed from machines do not pretend there are none.
            waitForDescription("Nodes, Non chargé")
            compose.onNodeWithContentDescription("Pouvoirs spéciaux, Non chargé").assertExists()
            open(AgentSection.Nodes)
            waitForText("Impossible de charger les nodes", substring = true)
            compose
                .onNodeWithContentDescription("Toutes les nodes VM, y compris futures")
                .assertIsNotEnabled()
            compose.onNodeWithContentDescription("Exécution native sur l’hôte").assertIsNotEnabled()
            // What the agent holds is listed as saved, not as a guessed machine with a status.
            compose.onNodeWithText("Exécution autorisée sur : $LOCAL_NODE_ID").assertExists()
            compose
                .onNodeWithContentDescription("Exécuter ses conversations sur Runner actuel")
                .assertDoesNotExist()

            fixture.failNodes.set(false)
            compose.onNodeWithText("Réessayer").performClick()
            waitForDescription("Exécuter ses conversations sur gpu-box")
            compose
                .onNodeWithText("Impossible de charger les nodes", substring = true)
                .assertDoesNotExist()
            compose
                .onNodeWithContentDescription("Toutes les nodes VM, y compris futures")
                .assertIsEnabled()
        }
    }

    @Test
    fun aHeldGrantOnAnUngrantablePluginIsShownAndCanBeRemoved() {
        // The Discord package gives agents nothing: an old grant on its bot is kept but useless.
        journey(
            "d3153f9f-724b-59c3-9614-ab78f231c4d3",
            prepare = { fixture ->
                fixture.agents.set(
                    fixture.agents.get().map {
                        if (it.id == "d3153f9f-724b-59c3-9614-ab78f231c4d3")
                            it.copy(plugins = listOf("bot"))
                        else it
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
                .performScrollTo()
                .assertExists()
            compose
                .onNodeWithContentDescription("Retirer #support")
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Indisponible").assertDoesNotExist()
            back()
            compose.onNodeWithText("⚠ 1 point à vérifier").assertDoesNotExist()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            assertEquals(
                JsonArray(emptyList()),
                fixture.saved.single().second["settings"]!!.jsonObject["plugins"],
            )
        }
    }

    @Test
    fun unsavedChangesAskBeforeLeavingAndAConflictKeepsTheDraft() {
        journey("d3153f9f-724b-59c3-9614-ab78f231c4d3") { fixture ->
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
                    if (it.id == "d3153f9f-724b-59c3-9614-ab78f231c4d3")
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
            assertEquals(
                "Release bot",
                body["settings"]!!.jsonObject["name"]!!.jsonPrimitive.content,
            )
            assertEquals(
                "Changée ailleurs",
                body["settings"]!!.jsonObject["description"]!!.jsonPrimitive.content,
            )
            // The untouched time limit takes the newer value instead of restoring the old one.
            assertEquals(45, body["settings"]!!.jsonObject["timeoutMinutes"]!!.jsonPrimitive.int)
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
    fun anInvalidTimeLimitStaysEditableUntilCorrected() {
        journey(release.id) { fixture ->
            open(AgentSection.Advanced)
            compose.onNodeWithText("Sans limite de temps").performClick()
            compose.onNodeWithText("Limite en minutes (1–720)").performTextReplacement("900")
            waitForText("Saisissez une limite entre 1 et 720 minutes.")
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            compose.onNodeWithText("Limite en minutes (1–720)").performTextReplacement("720")
            back()
            compose.onNodeWithText("Enregistrer").performClick()
            confirmReview()
            compose.waitUntil(15000) { fixture.saved.isNotEmpty() }
            assertEquals(
                720,
                fixture.saved
                    .last()
                    .second["settings"]!!
                    .jsonObject["timeoutMinutes"]!!
                    .jsonPrimitive
                    .int,
            )
        }
    }

    @Test
    fun creationRetriesKeepOneAgentAfterALostResponseAndAConflict() {
        journey("") { fixture ->
            fixture.loseCreation.set(true)
            // A new agent starts without access: the owner grants what it needs.
            compose.onNodeWithContentDescription("Projets, Aucun").assertExists()
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Nouvel agent")
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

            // Reloading keeps the draft; a further edit is saved from the created revision.
            compose.onNodeWithText("Recharger").performClick()
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Recharger").fetchSemanticsNodes().isEmpty()
            }
            open(AgentSection.Identity)
            compose.onNodeWithText("Nom").performTextReplacement("Agent de release")
            back()
            waitForEnabled("Créer l’agent")
            compose.onNodeWithText("Créer l’agent").performClick()
            compose.waitUntil(15000) { left.get() }
            val requests = fixture.saved.map { it.first }
            assertEquals(
                listOf(
                    "/agents",
                    "/agents",
                    "/agents/625c462a-fcff-5985-b84a-e212b3ac665d",
                    "/agents/625c462a-fcff-5985-b84a-e212b3ac665d",
                ),
                requests,
            )
            assertEquals("c1", fixture.saved[3].second["revision"]!!.jsonPrimitive.content)
            val creation = fixture.saved[0].second
            assertEquals(creation["creationId"], fixture.saved[1].second["creationId"])
            assertNotNull(creation["creationId"])
            assertFalse("revision" in creation)
            val access = creation["settings"]!!.jsonObject["access"]!!.jsonObject
            assertEquals(JsonArray(emptyList()), access["projects"])
            assertEquals(JsonArray(emptyList()), access["skills"])
            assertEquals(JsonArray(emptyList()), access["mcps"])
            assertFalse("github" in access)
            assertEquals(JsonArray(listOf(JsonPrimitive(LOCAL_NODE_ID))), access["nodes"])
            assertEquals(
                "Agent de release",
                fixture.saved[3].second["settings"]!!.jsonObject["name"]!!.jsonPrimitive.content,
            )
            assertEquals(
                listOf("Agent de release"),
                fixture.agents
                    .get()
                    .filter { it.id == "625c462a-fcff-5985-b84a-e212b3ac665d" }
                    .map { it.name },
            )
        }
    }
}
