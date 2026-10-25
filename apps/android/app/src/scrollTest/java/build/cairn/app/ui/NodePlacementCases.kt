package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class NodePlacementCases {
    protected open fun capture(name: String) {}

    /** Lets [millis] of main-thread time pass, as the page's polling sees it. */
    protected open fun elapse(millis: Long) = Thread.sleep(millis)

    @get:Rule val compose = createComposeRule()

    @Test
    fun retiringASwitchedNodeExplainsAgentsPlacementsAndTheOldRuntime() {
        var revoked = false
        var kept = false
        val previous =
            ExecutionNode(
                id = "vm",
                name = "Workstation",
                status = "online",
                agents = listOf(NodeAgent("a", "Builder"), NodeAgent("b", "Everywhere", true)),
            )
        val next =
            ExecutionNode(
                id = "native",
                name = "Workstation GPU",
                status = "online",
                slots = 1,
                capabilities = NodeCapabilities(backend = NATIVE_BACKEND, ublk = true),
            )
        compose.setContent {
            CairnTheme {
                RetireNode(previous, next, Workspace(), { kept = true }) { revoked = true }
            }
        }
        compose
            .onNodeWithText("Workstation GPU tourne maintenant comme node native", substring = true)
            .assertExists()
        // Only agents granted this node by name lose it; "all nodes" agents keep others.
        compose
            .onNodeWithText("Agents autorisés sur l’ancienne node : Builder.", substring = true)
            .assertExists()
        compose
            .onNodeWithText("reviennent au placement automatique", substring = true)
            .assertExists()
        compose
            .onNodeWithText("sudo docker rm -f cairn-execution-node", substring = true)
            .performScrollTo()
            .assertExists()
        capture("retire-switched-node")
        compose.onNodeWithText("Garder les deux pour l’instant").performScrollTo().performClick()
        compose.onNodeWithText("Révoquer l’ancienne node").performScrollTo().performClick()
        compose.runOnIdle {
            assertTrue(kept)
            assertTrue(revoked)
        }
    }

    @Test
    fun nativeAdministratorReviewShowsAffectedWorkAndRetainsFailedEffectiveMode() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<JsonObject>()
            val failed = AtomicBoolean(false)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when (path) {
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/installations/fixture/api/overview" -> "{}"
                                "/api/installations/fixture/api/nodes/native-callers" -> "[]"
                                "/api/installations/fixture/api/nodes/desk/native-mode" -> {
                                    val change =
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    writes += change
                                    if (change["confirm"]?.jsonPrimitive?.booleanOrNull == true) {
                                        failed.set(true)
                                        "{}"
                                    } else
                                        """{"previewId":"review","preview":{"mode":"administrator","trustedAccounts":["trusted"],"agents":[{"id":"builder","name":"Builder"}],"conversations":[{"id":"one","name":"Première conversation","status":"running"},{"id":"two","name":"Deuxième conversation","status":"running"}],"tasks":[{"id":"night","name":"Tâche nocturne"}]}}"""
                                }
                                "/api/installations/fixture/api/nodes" -> {
                                    val status = if (failed.get()) "failed" else "applied"
                                    val requested =
                                        if (failed.get())
                                            ",\"nativeModeRequest\":{\"mode\":\"administrator\",\"generation\":\"g1\"},\"nativeModeStatus\":\"failed\""
                                        else ""
                                    """[{"id":"desk","name":"Machine jetable","status":"online","accepting":true,"slots":2,"capabilities":{"backend":"native","nativeModeProtocol":1},"nativeModeObserved":{"mode":"protected","status":"$status"}$requested}]"""
                                }
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
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
                CairnTheme { if (state.session.authenticated) NodesScreen(vm, state) }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Passer en Natif administrateur")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Passer en Natif administrateur")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Première conversation · running")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Deuxième conversation · running").assertExists()
            compose.onNodeWithText("Tâche nocturne").performScrollTo().assertExists()
            compose.runOnIdle { assertEquals(1, writes.size) }
            capture("native-administrator-review")
            compose
                .onNodeWithText("Confirmer l’accès et relancer les exécutions")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Échec : nouveaux placements bloqués.", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Natif protégé").performScrollTo().assertExists()
            compose
                .onNodeWithText("privilèges actuels non confirmés", substring = true)
                .performScrollTo()
                .assertExists()
            compose.runOnIdle {
                assertEquals("administrator", writes.last()["mode"]?.jsonPrimitive?.content)
                assertEquals("review", writes.last()["previewId"]?.jsonPrimitive?.content)
                assertEquals(true, writes.last()["confirm"]?.jsonPrimitive?.boolean)
            }
        }
    }

    @Test
    fun nativeNodeEditorRequiresExplicitPeopleAndSupportsSixExecutionSlots() {
        var saved: NodeConfiguration? = null
        var keyboard: androidx.compose.ui.platform.SoftwareKeyboardController? = null
        val node =
            ExecutionNode(
                id = "native",
                name = "Desktop",
                status = "online",
                slots = 1,
                capabilities = NodeCapabilities(backend = "native"),
            )
        compose.setContent {
            keyboard = androidx.compose.ui.platform.LocalSoftwareKeyboardController.current
            CairnTheme {
                NodeEditor(
                    node,
                    Workspace(),
                    listOf(NativeCaller("trusted", "trusted@example.test")),
                    {},
                    { saved = it },
                )
            }
        }
        compose.onNodeWithText("Slots d’exécution").assertIsEnabled().performTextReplacement("6")
        compose.runOnIdle { keyboard?.hide() }
        compose.waitForIdle()
        compose
            .onNodeWithText("Tous les slots partagent ces budgets.", substring = true)
            .assertExists()
        compose.onNodeWithText("Réservation exclusive").assertDoesNotExist()
        compose.onNodeWithText("Mettez à jour le runtime", substring = true).assertDoesNotExist()
        compose.onNodeWithText("Personnes de confiance").performScrollTo().assertExists()
        compose
            .onNodeWithText("Activer l’exécution native")
            .performScrollTo()
            .performClick()
            .assertIsOn()
        compose.onNodeWithText("trusted@example.test").performScrollTo().performClick()
        capture("native-node-trust")
        compose.onNodeWithText("Enregistrer").performClick()
        compose.runOnIdle {
            assertEquals(true, saved?.nativeEnabled)
            assertEquals(listOf("trusted"), saved?.nativeAccounts)
            assertEquals(6, saved?.slots)
        }
    }

    @Test fun aNewNativeNodeIsConfiguredThenAsksWhichNativeAgentsMayUseIt() = nativeEnrollment(true)

    @Test
    fun dismissingTheConfigurationOfANewNativeNodeCancelsTheLastStep() = nativeEnrollment(false)

    private fun nativeEnrollment(save: Boolean) {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
            val enrolled = AtomicBoolean(false)
            val enrollmentConfigured = AtomicBoolean(false)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (
                            request.method in listOf("POST", "PUT") &&
                                path.startsWith("/api/installations/fixture/api/nodes")
                        ) {
                            val body = request.body.readUtf8().ifBlank { "{}" }
                            writes += path to wireJson.parseToJsonElement(body).jsonObject
                            if (request.method == "PUT" && path.endsWith("/nodes/desk"))
                                enrollmentConfigured.set(true)
                        }
                        val body =
                            when (path) {
                                "/api/installations/fixture/api/nodes/enrollments" -> {
                                    enrolled.set(true)
                                    """{"code":"native-enrollment","expiresAt":4102444800000,"installCommand":"curl https://fixture.invalid/native | bash","backend":"native","nodeId":"desk"}"""
                                }
                                "/api/installations/fixture/api/nodes/native-callers" ->
                                    """[{"id":"trusted","email":"trusted@example.test"}]"""
                                "/api/installations/fixture/api/nodes" ->
                                    if (enrollmentConfigured.get())
                                        """[{"id":"desk","name":"Desk","status":"online","accepting":true,"slots":1,"capabilities":{"backend":"native"},"agents":[]}]"""
                                    else if (enrolled.get())
                                        """[{"id":"unrelated","name":"Desk","status":"online","accepting":true,"slots":1,"capabilities":{"backend":"native"},"agents":[]},{"id":"desk","name":"Desk","status":"online","accepting":true,"slots":1,"capabilities":{"backend":"native"},"agents":[]}]"""
                                    else "[]"
                                "/api/installations/fixture/api/agents" ->
                                    """[{"id":"vm","name":"Agent VM","revision":"r1"},{"id":"native","name":"Agent natif","revision":"r1","access":{"backend":"native","nodes":[]}}]"""
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/overview" -> "{}"
                                "/api/installations/fixture/api/projects" -> """{"projects":[]}"""
                                else -> if (request.method == "PUT") "{}" else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
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
                CairnTheme { if (state.session.authenticated) NodesScreen(vm, state) }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Ajouter une machine").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Ajouter une machine").performScrollTo().performClick()
            compose.onNodeWithText("Nom de la machine").performTextInput("Desk")
            compose.onNodeWithText("Exécution native sur l’hôte").performClick()
            compose.onNodeWithText("Créer un code d’inscription").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("native-enrollment").fetchSemanticsNodes().isNotEmpty()
            }
            // Once the machine connects, it is configured first: native execution and people.
            repeat(15) {
                if (
                    compose
                        .onAllNodesWithText("Activer l’exécution native")
                        .fetchSemanticsNodes()
                        .isEmpty()
                ) {
                    compose.mainClock.advanceTimeBy(2_000)
                    elapse(2_000)
                    compose.waitForIdle()
                }
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Activer l’exécution native")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            if (!save) {
                // Dismissed without saving: like the web, the last step is cancelled.
                compose.onNodeWithContentDescription("Fermer").performClick()
                repeat(6) {
                    compose.mainClock.advanceTimeBy(2_000)
                    elapse(2_000)
                    compose.waitForIdle()
                }
                compose.onNodeWithText("Qui peut utiliser « Desk » ?").assertDoesNotExist()
                compose.onNodeWithText("Activer l’exécution native").assertDoesNotExist()
                assertTrue(
                    writes.none { it.first.startsWith("/api/installations/fixture/api/nodes/desk") }
                )
                return
            }
            compose.onNodeWithText("Activer l’exécution native").performScrollTo().performClick()
            compose.onNodeWithText("trusted@example.test").performScrollTo().performClick()
            compose.onNodeWithText("Enregistrer").performClick()

            // Then the same add-only step, offering native agents only.
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithText("Qui peut utiliser « Desk » ?")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNode(isOff() and hasText("Agent VM")).assertIsNotEnabled()
            compose.onNodeWithText("Agent en VM : réservé aux nodes VM").assertExists()
            compose.onNode(isOff() and hasText("Agent natif")).assertIsEnabled().performClick()
            compose.onNodeWithText("Terminer").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.any { it.first == "/api/installations/fixture/api/nodes/desk/agents" }
            }
            val configured = writes.first {
                it.first == "/api/installations/fixture/api/nodes/desk"
            }
            assertEquals(JsonPrimitive(true), configured.second["nativeEnabled"])
            val grant =
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/desk/agents" }
                    .second
            assertEquals(
                listOf("native"),
                grant["agentIds"]!!.jsonArray.map { it.jsonPrimitive.content },
            )
            assertEquals(JsonPrimitive(true), grant["add"])
            // The ublk storage policy applies to native disks; technical details name ublk.
            compose.onNodeWithText("Stockage").performScrollTo().assertExists()
            compose.onNodeWithText("Détails techniques").performScrollTo().performClick()
            compose.onNodeWithText("disque · ublk indisponible", substring = true).assertExists()
            compose
                .onNodeWithText(
                    "ublk indisponible : cette node a besoin d’un runtime natif à jour",
                    substring = true,
                )
                .assertExists()
            compose.onNodeWithText("Révoquer").performScrollTo().performClick()
            compose.onNodeWithText("grep ^/var/lib/cairn-native/", substring = true).assertExists()
            compose.onNodeWithText("Copier la commande").performScrollTo().assertIsDisplayed()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Confirmer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Confirmer").assertIsEnabled()
            compose
                .onNodeWithText("l’affiche et s’arrête avant de supprimer", substring = true)
                .assertExists()
        }
    }

    @Test fun suspendedChatTimelineShowsAStaticNoticeInsteadOfWorking() = suspendedTimeline(true)

    @Test fun suspendedRunTimelineAndResultDescribeThePause() = suspendedTimeline(false)

    private fun suspendedTimeline(chatPage: Boolean) {
        MockWebServer().use { server ->
            val reservation =
                ExclusiveReservation(
                    "reservation",
                    "node",
                    "owner",
                    "Compilation Android",
                    "acquired",
                )
            val run =
                Run(
                    "neighbor",
                    status = "running",
                    taskName = "Exécution voisine",
                    suspendedByExclusive = reservation,
                )
            val chat =
                Chat(
                    "chat",
                    title = "Conversation suspendue",
                    agentName = "Agent principal",
                    status = "running",
                    run = run,
                    suspendedByExclusive = reservation,
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val live =
                            when (path) {
                                "/api/installations/fixture/api/chats/stream" ->
                                    LiveState(chats = listOf(chat))
                                "/api/installations/fixture/api/chats/chat/stream" ->
                                    LiveState(chat = chat)
                                "/api/installations/fixture/api/runs/neighbor/stream" ->
                                    LiveState(run = run)
                                else -> null
                            }
                        if (live != null) {
                            val frame =
                                "event: batch\nid: 0\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), live, true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame)
                        }
                        val body =
                            when {
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations/fixture/api/overview" -> "{}"
                                path == "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(MAIN_AGENT_ID, "Agent principal"))
                                    )
                                path.endsWith("/models") -> """{"models":[]}"""
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = connectedViewModel()
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated) {
                        if (chatPage) ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
                        else RunScreen(vm, state, "neighbor", openRun = {})
                    }
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Suspendue (exclusivité) · Compilation Android")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("agent-working").assertDoesNotExist()
            compose.onNodeWithTag("chat-working-avatar").assertDoesNotExist()
            capture(if (chatPage) "exclusive-chat-timeline" else "exclusive-run-timeline")
            if (!chatPage) {
                compose.onNodeWithText("Résultat").performClick()
                compose.onNodeWithText("Exécution suspendue").assertExists()
                compose.onNodeWithText("Le travail est en cours").assertDoesNotExist()
            }
        }
    }

    @Test
    fun ownerCanInspectAndReleaseAnExclusiveReservationWithoutClaimingNeighborsResumed() {
        MockWebServer().use { server ->
            val releasing = AtomicBoolean(false)
            val releases = CopyOnWriteArrayList<RecordedRequest>()
            val opened = CopyOnWriteArrayList<String>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path ==
                                    "/api/installations/fixture/api/nodes/node/exclusive/reservation/release" -> {
                                    releases += request
                                    releasing.set(true)
                                    "{}"
                                }
                                path == "/api/installations/fixture/api/nodes" ->
                                    """[{"id":"node","name":"Serveur réservé","status":"online","accepting":true,"executionReady":true,"capabilities":{"kvm":true},"exclusiveReservations":true,"exclusiveReservation":{"id":"reservation","nodeId":"node","ownerRunId":"owner-run","reason":"Compilation Android","state":"${if (releasing.get()) "releasing" else "acquired"}","createdAt":100,"expiresAt":4102444800000,"waitUntil":4102444800000,"pausedRunIds":["neighbor-one","neighbor-two"],"minimumAvailableMemoryMiB":4096}}]"""
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations/fixture/api/overview" -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = connectedViewModel()
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme { if (state.session.authenticated) NodesScreen(vm, state, opened::add) }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Exclusivité acquise").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Compilation Android").assertExists()
            compose.onNodeWithText("2 tâches suspendues").assertExists()
            compose.onNodeWithText("Expire le", substring = true).assertExists()
            compose.onNodeWithText("RAM disponible demandée : 4 Gio").assertExists()
            compose.onNodeWithText("Ouvrir l’exécution").performScrollTo().performClick()
            assertEquals(listOf("owner-run"), opened.toList())
            capture("exclusive-reservation")
            compose.onNodeWithText("Libérer la réservation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                releases.size == 1 &&
                    compose
                        .onAllNodesWithText("Reprise des autres tâches")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals("POST", releases.single().method)
            assertEquals("fixture", releases.single().getHeader("X-CSRF-Token"))
            compose.onNodeWithText("Libérer la réservation").assertIsNotEnabled()
            compose.onNodeWithText("2 tâches suspendues").assertExists()
        }
    }

    @Test
    fun suspendedRunKeepsStorageFailuresVisibleAndExplainsAutomaticResume() {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val body =
                            when (request.path!!.substringBefore('?')) {
                                "/api/installations/fixture/api/nodes/placement/neighbor" ->
                                    """{"nodes":[{"id":"node","name":"Serveur réservé","status":"online"}]}"""
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/overview" -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = connectedViewModel()
            val run =
                mutableStateOf(
                    Run(
                        id = "neighbor",
                        status = "running",
                        nodeId = LOCAL_NODE_ID,
                        suspendedByExclusive =
                            ExclusiveReservation(
                                id = "reservation",
                                nodeId = "node",
                                ownerRunId = "owner-run",
                                reason = "Compilation Android",
                                state = "acquired",
                                expiresAt = 4102444800000,
                            ),
                    )
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated)
                        Page {
                            RunCard(run.value) {}
                            NodePlacement(vm, run.value)
                        }
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Suspendue pour une réservation exclusive")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Suspendue (exclusivité)").assertExists()
            compose.runOnIdle { run.value = run.value.copy(movementError = "Transfert impossible") }
            compose.onNodeWithText("Déplacement en échec").assertExists()
            compose.onNodeWithText("Node : Runner du master").performClick()
            compose.onNodeWithText("Compilation Android").assertExists()
            compose.onNodeWithText("Réservation jusqu’au", substring = true).assertExists()
            compose
                .onNodeWithText(
                    "La reprise est automatique après la libération, dès que la node est disponible."
                )
                .assertExists()
            compose.onNodeWithText("Transfert impossible").assertExists()
            capture("exclusive-suspended-run")
            compose.onNodeWithContentDescription("Fermer Node d’exécution").performClick()
            compose.runOnIdle { run.value = run.value.copy(status = "failed") }
            compose.onNodeWithText("Échec").assertExists()
            compose.onNodeWithText("Suspendue (exclusivité)").assertDoesNotExist()
            compose.onNodeWithText("Suspendue pour une réservation exclusive").assertDoesNotExist()
        }
    }

    @Test
    fun aRunningNativeConversationShowsItsStorageSynchronization() {
        MockWebServer().use { server ->
            val privilegeStatus = java.util.concurrent.atomic.AtomicReference("pending")
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val body =
                            when (request.path!!.substringBefore('?')) {
                                "/api/installations/fixture/api/nodes/placement/native" ->
                                    """{"nodes":[{"id":"desk","name":"Bureau GPU","status":"online","capabilities":{"backend":"native","ublk":true},"nativeModeStatus":"${privilegeStatus.get()}","nativeModeObserved":{"mode":"${if (privilegeStatus.get() == "applied") "administrator" else "protected"}"}}]}"""
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/overview" -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = connectedViewModel()
            // A legacy native file workspace is only saved once execution stops.
            val run =
                mutableStateOf(
                    Run(
                        id = "native",
                        status = "running",
                        nodeId = "desk",
                        executionBackend = "native",
                        storage = RunStorage(mode = "native", dirtyBytes = 0),
                        backup = NodeBackup(capturedAt = System.currentTimeMillis() - 120_000),
                    )
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated) Page { NodePlacement(vm, run.value) }
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Travail local · sauvegardé après l’arrêt")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            // Native ublk disks synchronize while running, like VM disks.
            compose.runOnIdle {
                run.value = run.value.copy(storage = RunStorage(mode = "on-demand", dirtyBytes = 0))
            }
            compose.onNodeWithText("Travail local · sauvegardé après l’arrêt").assertDoesNotExist()
            compose.onNodeWithText("Synchronisé il y a 2 min").assertExists()
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Node : Bureau GPU").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Node : Bureau GPU").performClick()
            compose
                .onNodeWithText("Dernière exécution : Natif protégé · GPU partagé")
                .assertExists()
            compose.onNodeWithText("Basculement en cours", substring = true).assertExists()
            privilegeStatus.set("failed")
            compose.waitUntil(20000) {
                compose.mainClock.advanceTimeBy(500)
                elapse(500)
                compose
                    .onAllNodesWithText("Échec du basculement", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            privilegeStatus.set("applied")
            compose.waitUntil(20000) {
                compose.mainClock.advanceTimeBy(500)
                elapse(500)
                compose
                    .onAllNodesWithText("Mode de node confirmé : Natif administrateur")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Dernière exécution : Natif protégé · GPU partagé")
                .assertExists()
            compose.onNodeWithText("Fichiers chargés à la demande").assertExists()
            compose.onNodeWithText("Sauvegarde entre les tours").assertDoesNotExist()
            capture("native-run-synchronized")
        }
    }

    private fun connectedViewModel(): CairnViewModel =
        CairnViewModel(
            ApplicationProvider.getApplicationContext<Application>(),
            object : SessionVault {
                private val values = mutableMapOf<String, String>()

                override fun read(origin: String) = values[origin]

                override fun write(origin: String, cookie: String?) {
                    if (cookie == null) values.remove(origin) else values[origin] = cookie
                }
            },
            beaconOrigin = "",
        )

    @Test
    fun ownerCanPreferOrPinAnAuthorizedNodeAndSeeSynchronizationAge() {
        MockWebServer().use { server ->
            val saved = CopyOnWriteArrayList<JsonObject>()
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path == "/api/installations/fixture/api/nodes/placement/run" &&
                                    request.method == "PUT" -> {
                                    saved +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    "{}"
                                }
                                path == "/api/installations/fixture/api/nodes/placement/run" ->
                                    """{"nodes":[{"id":"node","name":"Mon serveur","status":"online"},{"id":"other","name":"Autre serveur","status":"online"}],"pinnedNodeId":null,"preferredNodeId":null}"""
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations/fixture/api/overview" -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
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
                    if (state.session.authenticated)
                        Page {
                            NodePlacement(
                                vm,
                                Run(
                                    id = "run",
                                    status = "succeeded",
                                    nodeId = "node",
                                    resources = NodeResources(2, 4096, 32768),
                                    storage =
                                        RunStorage(
                                            mode = "on-demand",
                                            localBytes = 191392768,
                                            dirtyBytes = 183890000,
                                            dirtySince = System.currentTimeMillis() - 60000,
                                        ),
                                    backup =
                                        NodeBackup(
                                            capturedAt = System.currentTimeMillis() - 120000,
                                            status = "ready",
                                        ),
                                ),
                            )
                        }
                }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Node : Mon serveur", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Node : Mon serveur").performClick()
            compose.onNodeWithText("Fichiers chargés à la demande").assertExists()
            compose
                .onNodeWithText("182,5 Mio sur cette node · 175,4 Mio non synchronisés")
                .assertExists()
            capture("execution-sheet")
            compose.onNodeWithText("Enregistrer").performScrollTo().assertIsNotEnabled()
            compose.onNodeWithText("Préférer").performClick()
            compose.onNodeWithText("Enregistrer").performScrollTo().performClick()
            // Receipt by the server precedes the response and its UI confirmation.
            compose.waitUntil(10000) {
                saved.size == 1 &&
                    compose
                        .onAllNodesWithText("Préférence enregistrée", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals("node", saved[0]["preferredNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull, saved[0]["pinnedNodeId"])
            compose.onNodeWithText("Fixer").performScrollTo().performClick()
            compose.onNodeWithText("Enregistrer").performScrollTo().performClick()
            compose.waitUntil(10000) {
                saved.size == 2 &&
                    compose
                        .onAllNodesWithText("Préférence enregistrée", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
            }
            assertEquals("node", saved[1]["pinnedNodeId"]?.jsonPrimitive?.content)
            assertEquals(JsonNull, saved[1]["preferredNodeId"])
            compose.onNodeWithText("Dernière synchronisation", substring = true).assertExists()
            compose.onNodeWithText("Préférence enregistrée", substring = true).assertExists()
            // Without a recorded provider session there is nothing to resume elsewhere yet.
            compose.onNodeWithText("Déplacer…").performScrollTo().performClick()
            compose.onNodeWithText("Déplacer maintenant").performScrollTo().assertIsNotEnabled()
        }
    }

    @Test
    fun backendMigrationRequiresConfirmationAndFollowsItsCompletion() {
        MockWebServer().use { server ->
            val submitted = CopyOnWriteArrayList<JsonObject>()
            val completed = AtomicBoolean(false)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val body =
                            when {
                                path.endsWith("/backend-migration") && request.method == "POST" -> {
                                    submitted +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    "{}"
                                }
                                path.endsWith("/backend-migration") ->
                                    """{"required":${!completed.get()},"toBackend":"native","pending":${submitted.isNotEmpty() && !completed.get()},"nodes":[{"id":"gpu","name":"Desktop GPU"}]}"""
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path.endsWith("/overview") -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
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
                CairnTheme { if (state.session.authenticated) Page { BackendMigration(vm, "run") } }
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Migrer cette conversation…")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Migrer cette conversation…").performScrollTo().performClick()
            compose
                .onNodeWithText("Le prochain message ouvre une nouvelle session", substring = true)
                .assertExists()
            assertTrue(submitted.isEmpty())
            capture("backend-migration-confirm")
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) { submitted.isNotEmpty() }
            assertEquals("native", submitted[0]["backend"]?.jsonPrimitive?.content)
            assertEquals("gpu", submitted[0]["nodeId"]?.jsonPrimitive?.content)
            assertEquals(true, submitted[0]["confirm"]?.jsonPrimitive?.boolean)
            completed.set(true)
            elapse(2000)
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Migration terminée.", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
        }
    }

    @Test
    fun ownerCanEnrollConfigureAndRevokeANode() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<Pair<String, JsonObject>>()
            var revoked = false
            var configured = false
            var granted = false
            var enrolled = false
            var cleaned = false
            // Granted from its agent page in another window while the last step is open.
            var grantedElsewhere = false
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (
                            request.method in listOf("POST", "PUT") &&
                                path.startsWith("/api/installations/fixture/api/nodes")
                        ) {
                            val body = request.body.readUtf8().ifBlank { "{}" }
                            writes += path to wireJson.parseToJsonElement(body).jsonObject
                        }
                        val body =
                            when {
                                path == "/api/installations/fixture/api/nodes/enrollments" -> {
                                    enrolled = true
                                    """{"code":"fixture-enrollment","nodeId":"fresh","expiresAt":4102444800000,"installCommand":"curl https://fixture.invalid/install | bash"}"""
                                }
                                path == "/api/installations/fixture/api/nodes/node/revoke" -> {
                                    revoked = true
                                    """{"releasedPlacements":0}"""
                                }
                                path == "/api/installations/fixture/api/nodes/node" -> {
                                    configured = true
                                    "{}"
                                }
                                path == "/api/installations/fixture/api/nodes/settings" -> "{}"
                                path == "/api/installations/fixture/api/nodes/fresh/agents" -> {
                                    granted = true
                                    "{}"
                                }
                                path ==
                                    "/api/installations/fixture/api/nodes/node/stale-disks/delete" -> {
                                    cleaned = true
                                    """{"freedMiB":2048,"failed":0}"""
                                }
                                path == "/api/installations/fixture/api/nodes" ->
                                    """[{"id":"node","name":"Serveur test","status":"${if (revoked) "revoked" else "online"}","revoked":$revoked,"accepting":true,"limits":{"cpu":4,"memoryMiB":8192,"diskMiB":65536},"storage":{"memoryCacheMiB":512},"agents":[],"exclusiveAgents":[],"staleDisks":{"count":${if (cleaned) 0 else 1},"diskMiB":2048}}${if (enrolled) """,{"id":"fresh","name":"Nouvelle node","status":"online","accepting":true,"agents":[{"id":"other","name":"Agent autorisé"},{"id":"main","name":"Agent global","allNodes":true}${if (grantedElsewhere) """,{"id":"late","name":"Agent tardif"}""" else ""}${if (granted) """,{"id":"agent","name":"Agent test"}""" else ""}]}""" else ""}]"""
                                path == "/api/installations/fixture/api/agents" ->
                                    """[{"id":"agent","name":"Agent test"},{"id":"other","name":"Agent autorisé","access":{"nodes":["fresh"]}},{"id":"late","name":"Agent tardif"},{"id":"main","name":"Agent global","access":{"nodes":null}},{"id":"native","name":"Agent natif","access":{"backend":"native","nodes":[]}}]"""
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations/fixture/api/overview" -> "{}"
                                else ->
                                    if (
                                        request.requestUrl
                                            ?.encodedPath
                                            ?.endsWith("/api/projects") == true
                                    )
                                        """{"projects":[]}"""
                                    else "[]"
                            }
                        return MockResponse().setBody(body).apply {
                            // Keep the save busy while the subsequent node refresh is in flight.
                            if (path == "/api/installations/fixture/api/nodes" && configured)
                                setBodyDelay(1, java.util.concurrent.TimeUnit.SECONDS)
                            else if (
                                path == "/api/installations/fixture/api/nodes" &&
                                    writes.any {
                                        it.first ==
                                            "/api/installations/fixture/api/nodes/node/storage"
                                    }
                            )
                                setBodyDelay(200, java.util.concurrent.TimeUnit.MILLISECONDS)
                        }
                    }
                }
            server.start()
            val vault =
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
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
                CairnTheme { if (state.session.authenticated) NodesScreen(vm, state) }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Ajouter une machine").fetchSemanticsNodes().isNotEmpty()
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Serveur test").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText(
                    "Aucun agent ne peut encore utiliser cette machine.",
                    substring = true,
                )
                .assertExists()
            capture("nodes")
            compose.onNodeWithText("Stockage").performScrollTo().performClick()
            compose.onNodeWithText("Cache propre (Mio)").performTextReplacement("51200")
            capture("storage")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                writes.any { it.first == "/api/installations/fixture/api/nodes/node/storage" } &&
                    compose
                        .onAllNodesWithText("Stockage · Serveur test")
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            val storage =
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/node/storage" }
                    .second
            assertEquals(51200, storage["cacheMiB"]!!.jsonPrimitive.int)
            // The memory cache is edited on the web; saving here keeps its value.
            assertEquals(512, storage["memoryCacheMiB"]!!.jsonPrimitive.int)
            // Agent access is read-only here: it is set on each agent's page.
            compose.onNodeWithText("Choisir les agents").assertDoesNotExist()
            compose.onNodeWithText("Aucun agent ne peut exécuter ici.").assertExists()
            compose.onNodeWithText("Aucun agent ne peut la réserver en exclusivité.").assertExists()
            compose.onNodeWithText("Gérer l’accès depuis les agents ›").assertExists()
            compose.onNodeWithText("Anciens disques : 2 Gio (1 conversation)").assertExists()
            compose.onNodeWithText("Libérer").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Confirmer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                writes.any {
                    it.first == "/api/installations/fixture/api/nodes/node/stale-disks/delete"
                } &&
                    compose
                        .onAllNodesWithText("Anciens disques", substring = true)
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            compose
                .onNodeWithText("Avancé : synchronisation et délais")
                .performScrollTo()
                .performClick()
            compose.onNodeWithText("Configurer la synchronisation").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Intervalle (secondes)")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Intervalle (secondes)").performTextReplacement("30")
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                writes.any { it.first == "/api/installations/fixture/api/nodes/settings" } &&
                    compose.onAllNodesWithText("Synchronisation S3").fetchSemanticsNodes().isEmpty()
            }
            assertEquals(
                30,
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/settings" }
                    .second["intervalSeconds"]!!
                    .jsonPrimitive
                    .int,
            )
            compose.onNodeWithText("Configurer", substring = false).performScrollTo().performClick()
            // The previous save may close its sheet before releasing busy.
            // Wait for the actual editor before sending text or toggle gestures.
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Budget CPU partagé").performTextReplacement("3")
            compose.onNodeWithText("Slots d’exécution").performTextReplacement("12")
            compose.onNodeWithText("Autoriser Agent test").assertDoesNotExist()
            compose.onNodeWithText("Enregistrer").performClick()
            compose.waitUntil(10000) {
                writes.any { it.first == "/api/installations/fixture/api/nodes/node" }
            }
            assertEquals(
                3,
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/node" }
                    .second["limits"]!!
                    .jsonObject["cpu"]!!
                    .jsonPrimitive
                    .int,
            )
            // Exclusive grants stay with the agents: the node never sends them.
            assertFalse(
                "exclusiveAgents" in
                    writes.first { it.first == "/api/installations/fixture/api/nodes/node" }.second
            )
            compose.onNodeWithText("Révoquer", substring = false).performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasText("Confirmer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Confirmer").assertIsEnabled().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Afficher les machines révoquées (1)")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose
                .onNodeWithText("Afficher les machines révoquées (1)")
                .performScrollTo()
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Révoquée").fetchSemanticsNodes().isNotEmpty()
            }
            assertTrue(
                writes.any { it.first == "/api/installations/fixture/api/nodes/node/revoke" }
            )
            compose.onNodeWithText("Ajouter une machine").performScrollTo().performClick()
            compose.onNodeWithText("Nom de la machine").performTextInput("Nouvelle node")
            compose.onNodeWithText("Créer un code d’inscription").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("fixture-enrollment").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(
                "Nouvelle node",
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/enrollments" }
                    .second["name"]
                    ?.jsonPrimitive
                    ?.content,
            )
            // Once the machine connects, the last step asks who may use it, nothing pre-checked
            // except agents already allowed on every node.
            // The page polls every 10 seconds.
            repeat(15) {
                if (
                    compose
                        .onAllNodesWithText("Qui peut utiliser « Nouvelle node » ?")
                        .fetchSemanticsNodes()
                        .isEmpty()
                ) {
                    compose.mainClock.advanceTimeBy(2_000)
                    elapse(2_000)
                    compose.waitForIdle()
                }
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Qui peut utiliser « Nouvelle node » ?")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            capture("first-agents")
            compose.onNodeWithText("Autorisé sur toutes les nodes").assertExists()
            // An agent already allowed here stays allowed: finishing never revokes it.
            compose.onNodeWithText("Déjà autorisé sur cette machine").assertExists()
            compose.onNode(isOn() and hasText("Agent autorisé")).assertIsNotEnabled()
            compose.onNodeWithText("Plus tard").assertExists()
            // An agent of another execution backend cannot run on this VM node.
            compose.onNode(isOff() and hasText("Agent natif")).assertIsNotEnabled()
            compose
                .onNodeWithText("Agent en exécution native : réservé aux nodes natives")
                .assertExists()
            compose.onNodeWithText("Agent test").performScrollTo().performClick()
            // Another window grants an agent meanwhile; the next refresh brings it in.
            grantedElsewhere = true
            compose.onNode(isOff() and hasText("Agent tardif")).assertIsEnabled()
            repeat(6) {
                compose.mainClock.advanceTimeBy(2_000)
                elapse(2_000)
                compose.waitForIdle()
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Déjà autorisé sur cette machine")
                    .fetchSemanticsNodes()
                    .size == 2
            }
            compose.onNode(isOn() and hasText("Agent tardif")).assertIsNotEnabled()
            compose.onNodeWithText("Terminer").performScrollTo().performClick()
            compose.waitUntil(10000) {
                writes.any { it.first == "/api/installations/fixture/api/nodes/fresh/agents" } &&
                    compose
                        .onAllNodesWithText("Qui peut utiliser « Nouvelle node » ?")
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
            // Only the newly chosen agent is sent, as a grant: the agents already allowed here,
            // including the one allowed meanwhile, are never revoked.
            val grant =
                writes
                    .first { it.first == "/api/installations/fixture/api/nodes/fresh/agents" }
                    .second
            assertEquals(
                listOf("agent"),
                grant["agentIds"]!!.jsonArray.map { it.jsonPrimitive.content },
            )
            assertEquals(true, grant["add"]?.jsonPrimitive?.booleanOrNull)
        }
    }
}
