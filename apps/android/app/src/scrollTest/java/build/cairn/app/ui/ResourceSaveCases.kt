package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/**
 * Native editors must retain the committed identity through refresh and response failures. Agent
 * creation is covered by `AgentPageCases`.
 */
abstract class ResourceSaveCases {
    @get:Rule val compose = createComposeRule()

    protected open fun captureFailure(name: String) = Unit

    @Test fun taskRefreshFailureKeepsTheCreatedMission() = saveAfterFailure("tasks", false)

    @Test fun projectRefreshFailureKeepsTheCreatedProject() = saveAfterFailure("projects", false)

    @Test
    fun pluginImportRefreshFailureKeepsTheImportedProject() = saveAfterFailure("import", false)

    @Test
    fun lostPluginImportResponseRecoversTheProjectBeforeUpdatingEdits() =
        saveAfterFailure("import", true)

    @Test
    fun lostTaskResponseReplaysTheOriginalCreationThenUpdatesTheEditedMission() =
        saveAfterFailure("tasks", true)

    @Test
    fun lostProjectResponseReplaysTheOriginalCreationThenUpdatesTheEditedProject() =
        saveAfterFailure("projects", true)

    @Test
    fun rejectedReplayStillAppliesTheLatestMissionEdits() =
        saveAfterFailure("tasks", true, rejectReplay = true)

    @Test
    fun malformedResponseThenRejectedReplayKeepsTheProjectSourceAndIdentity() =
        saveAfterFailure("projects", false, rejectReplay = true, malformedResponse = true)

    @Test
    fun rejectedUpdateAfterRefreshFailureNamesTheCreatedProject() =
        saveAfterFailure("projects", false, rejectReplay = true)

    @Test
    fun restoredTaskEditorKeepsItsCommittedIdentity() =
        saveAfterFailure("tasks", false, restore = true)

    @Test
    fun restoredTaskEditorReplaysItsUncertainCreation() =
        saveAfterFailure("tasks", true, restore = true)

    @Test
    fun projectBaseBranchCanBeReplaced() = saveAfterFailure("projects", false, editBranch = true)

    private fun saveAfterFailure(
        kind: String,
        loseResponse: Boolean,
        rejectReplay: Boolean = false,
        restore: Boolean = false,
        malformedResponse: Boolean = false,
        editBranch: Boolean = false,
    ) {
        val savedId = if (kind == "tasks") "saved" else "00000000-0000-4000-8000-000000000005"
        val uncertainResponse = loseResponse || malformedResponse
        val resourceKind = if (kind == "import") "projects" else kind
        val mutations = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val failRefresh = AtomicBoolean(false)
        val closed = AtomicBoolean(false)
        val created = AtomicBoolean(false)
        var committed: JsonObject? = null
        val recoveryTimeline = CopyOnWriteArrayList<String>()
        val startedAt = System.nanoTime()

        fun traceRecovery(message: String) {
            val elapsedMillis = (System.nanoTime() - startedAt) / 1_000_000
            recoveryTimeline += "$elapsedMillis ms: $message"
        }

        val allowRecoveryProbe = CountDownLatch(1)
        val recoveryRefreshStarted = CountDownLatch(1)
        val allowRecoveryRefresh = CountDownLatch(1)
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val resourcePath = "/api/installations/fixture/api/$resourceKind"
                        if (
                            request.method in listOf("POST", "PUT") &&
                                (path == resourcePath ||
                                    path == "$resourcePath/$savedId" ||
                                    path == "$resourcePath/import")
                        ) {
                            val value =
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            mutations += request.method!! to value
                            if (loseResponse) traceRecovery("${request.method} $path")
                            if (rejectReplay && mutations.size == 2)
                                return MockResponse()
                                    .setResponseCode(429)
                                    .setBody("""{"error":"Réessayez"}""")
                            val first = !created.getAndSet(true)
                            if (first) committed = value
                            if (first && malformedResponse) return json("{}")
                            if (first && loseResponse)
                                return MockResponse()
                                    .setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST)
                            failRefresh.set(first)
                            val stored = if (request.method == "POST") committed!! else value
                            val projectPath =
                                if (kind == "import")
                                    mapOf("path" to JsonPrimitive("/fixtures/imported"))
                                else emptyMap()
                            val settings =
                                if (
                                    kind == "projects" ||
                                        request.method == "PUT" && resourceKind == "projects"
                                )
                                    stored["settings"]!!.jsonObject
                                else stored
                            val output =
                                if (resourceKind == "projects")
                                    JsonObject(
                                        settings.filterKeys {
                                            it in
                                                setOf(
                                                    "name",
                                                    "path",
                                                    "description",
                                                    "baseBranch",
                                                    "sourceMode",
                                                )
                                        } +
                                            projectPath +
                                            mapOf(
                                                "id" to JsonPrimitive(savedId),
                                                "origin" to JsonPrimitive(""),
                                                "revision" to
                                                    JsonPrimitive("revision-${mutations.size}"),
                                                "createdAt" to JsonPrimitive(0L),
                                                "sourceMode" to
                                                    (settings["sourceMode"]
                                                        ?: JsonPrimitive("remote")),
                                            )
                                    )
                                else JsonObject(stored + ("id" to JsonPrimitive(savedId)))
                            return json(output.toString())
                        }
                        if (loseResponse && created.get() && request.method == "GET") {
                            if (path.endsWith("/overview")) {
                                traceRecovery("GET /overview waiting for initial-error capture")
                                check(allowRecoveryProbe.await(20, TimeUnit.SECONDS))
                                traceRecovery("GET /overview released")
                            }
                            if (path.endsWith("/tasks") && mutations.size == 1) {
                                traceRecovery("GET /tasks recovery refresh waiting")
                                recoveryRefreshStarted.countDown()
                                check(allowRecoveryRefresh.await(20, TimeUnit.SECONDS))
                                traceRecovery("GET /tasks recovery refresh released")
                            }
                        }
                        if (path.endsWith("/tasks") && failRefresh.get())
                            return MockResponse()
                                .setResponseCode(409)
                                .setBody("""{"error":"Lecture refusée"}""")
                        return json(
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                // A plugin with a project source and one enabled account.
                                path.endsWith("/api/plugins") ->
                                    """{"packages":[{"id":"github","status":"active","active":"1.0.0","manifest":{"id":"github","name":"GitHub","projectSources":[{"name":"repositories","title":"GitHub repository"}]}}],"connections":[{"id":"acct","pluginId":"github","name":"octo","enabled":true}]}"""
                                path.endsWith("/plugins/github/projects") ->
                                    """{"items":[{"id":"owner/repository","name":"owner/repository","description":"","defaultBranch":"main","badges":["Private"],"imported":false}],"nextPage":null}"""
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                path.endsWith("/agents") ->
                                    """{"agents":[{"access":{"backend":"firecracker","delegation":false,"installation":false,"mcpTools":{},"mcps":null,"nodes":["00000000-0000-4000-8000-000000000002"],"projects":null,"sandbox":"yolo","skills":null},"createdAt":0,"description":"","exclusiveNodes":[],"id":"d3153f9f-724b-59c3-9614-ab78f231c4d3","instructions":"","model":"","name":"Agent","plugins":[],"provider":"codex","reasoning":"high","revision":"fixture-revision","timeoutMinutes":0}]}"""
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
                                    else "[]"
                            }
                        )
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            // Resolving the URL starts the server; StrictMode rejects that on the device main
            // thread.
            val origin = server.url("/").toString()
            val models = ViewModelStore().apply { put("fixture", vm) }
            val restoration = StateRestorationTester(compose)
            restoration.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (vm.state.value.installation == null) vm.connect(origin)
                }
                val state = vm.state.collectAsState().value
                CairnTheme {
                    if (state.installation != null && !closed.get()) {
                        when (kind) {
                            "tasks" ->
                                TaskEditor(
                                    vm,
                                    state,
                                    Task(name = "Original", prompt = "Brief", agentId = "agent"),
                                    close = { closed.set(true) },
                                )
                            "projects",
                            "import" ->
                                ProjectEditor(
                                    vm,
                                    state,
                                    Project(
                                        name = "Original",
                                        path = "/fixtures/project",
                                        sourceMode = if (kind == "import") "local" else "remote",
                                    ),
                                    close = { closed.set(true) },
                                )
                            else -> error("Unknown editor: $kind")
                        }
                    }
                }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                waitFor { vm.state.value.installation != null && !vm.state.value.busy }
                if (kind == "import") {
                    compose.waitUntil(20000) {
                        compose
                            .onAllNodesWithText("Dossier sur le serveur")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Dossier sur le serveur").performClick()
                    compose.onNodeWithText("GitHub repository").performClick()
                    compose.waitUntil(20000) {
                        compose
                            .onAllNodesWithText("owner/repository")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("owner/repository").performScrollTo().performClick()
                }
                if (editBranch) {
                    compose
                        .onNodeWithText("Branche de base")
                        .performScrollTo()
                        .performTextReplacement("")
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    compose
                        .onNodeWithText("Branche de base")
                        .performTextReplacement("invalid branch")
                    compose.onNodeWithText("Enregistrer").performClick()
                    compose
                        .onNodeWithText("Saisissez un nom de branche de base valide.")
                        .assertExists()
                    assertTrue(mutations.isEmpty())
                    compose.onNodeWithText("Branche de base").performTextReplacement("develop")
                }
                compose
                    .onNode(hasScrollAction() and hasAnyDescendant(hasText("Nom")))
                    .performTouchInput { swipeUp() }
                compose.onNodeWithText("Enregistrer").performClick()
                waitFor { mutations.isNotEmpty() && !vm.state.value.busy }
                assertFalse(closed.get())
                if (!uncertainResponse) {
                    val label = if (kind == "tasks") "Mission" else "Projet"
                    compose
                        .onNodeWithText(
                            "$label enregistré${if (kind == "tasks") "e" else ""}, mais le rechargement a échoué. Réessayez."
                        )
                        .assertIsDisplayed()
                }
                if (uncertainResponse)
                    compose.onNodeWithText("Sauvegarde", substring = true).assertIsDisplayed()
                captureFailure("$kind-${if (uncertainResponse) "uncertain" else "reload"}")
                if (uncertainResponse && kind == "projects")
                    compose.onNodeWithText("Dossier sur le serveur").assertIsNotEnabled()
                if (loseResponse) {
                    // A lost response also starts background recovery. Hold its refresh so this
                    // test exercises the disabled Save, then retry only after recovery completes.
                    assertTrue(vm.state.value.connectionLost)
                    traceRecovery(
                        "Initial failure: busy=${vm.state.value.busy}; connectionLost=${vm.state.value.connectionLost}; mutations=${mutations.size}"
                    )
                    allowRecoveryProbe.countDown()
                    waitFor { recoveryRefreshStarted.count == 0L && vm.state.value.busy }
                    compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
                    compose.onNodeWithText("Enregistrer").performTouchInput { click() }
                    compose.runOnIdle {
                        assertEquals("Recovery must not replay the creation", 1, mutations.size)
                        assertFalse(closed.get())
                        traceRecovery(
                            "Save disabled and touched: busy=${vm.state.value.busy}; mutations=${mutations.size}"
                        )
                    }
                    allowRecoveryRefresh.countDown()
                    waitFor { !vm.state.value.connectionLost && !vm.state.value.busy }
                    compose.onNodeWithText("Enregistrer").assertIsEnabled()
                    assertEquals(1, mutations.size)
                    traceRecovery(
                        "Recovery complete: busy=${vm.state.value.busy}; connectionLost=${vm.state.value.connectionLost}; mutations=${mutations.size}"
                    )
                }
                if (restore) {
                    restoration.emulateSavedInstanceStateRestore()
                    if (!uncertainResponse)
                        compose
                            .onNodeWithText(
                                "Mission enregistrée, mais le rechargement a échoué. Réessayez."
                            )
                            .assertExists()
                }
                if (rejectReplay) {
                    compose.onNodeWithText("Enregistrer").performClick()
                    waitFor { mutations.size == 2 && !vm.state.value.busy }
                    if (!uncertainResponse) {
                        compose
                            .onNodeWithText(
                                "Projet enregistré, mais les dernières modifications n’ont pas été enregistrées. Réessayez."
                            )
                            .assertIsDisplayed()
                        captureFailure("$kind-update")
                    }
                    if (kind == "projects" && uncertainResponse)
                        compose.onNodeWithText("Dossier sur le serveur").assertIsNotEnabled()
                }
                compose.onNodeWithText("Nom").performScrollTo().performTextReplacement("Modifié")
                compose.onNodeWithText("Enregistrer").performClick()
                waitFor { closed.get() && !vm.state.value.busy }
                val expected =
                    when {
                        rejectReplay && !uncertainResponse -> listOf("POST", "PUT", "PUT")
                        rejectReplay -> listOf("POST", "POST", "POST", "PUT")
                        uncertainResponse -> listOf("POST", "POST", "PUT")
                        else -> listOf("POST", "PUT")
                    }
                assertEquals(expected, mutations.map { it.first })
                assertEquals(
                    "Modifié",
                    (if (resourceKind == "projects")
                            mutations.last().second["settings"]!!.jsonObject
                        else mutations.last().second)["name"]!!
                        .jsonPrimitive
                        .content,
                )
                if (editBranch) {
                    assertEquals(
                        "develop",
                        mutations
                            .first()
                            .second["settings"]!!
                            .jsonObject["baseBranch"]!!
                            .jsonPrimitive
                            .content,
                    )
                    assertEquals(
                        "develop",
                        mutations
                            .last()
                            .second["settings"]!!
                            .jsonObject["baseBranch"]!!
                            .jsonPrimitive
                            .content,
                    )
                }
                if (kind == "import") {
                    val import = mutations.first().second
                    assertEquals("github", import["pluginId"]!!.jsonPrimitive.content)
                    assertEquals("repositories", import["source"]!!.jsonPrimitive.content)
                    assertEquals("acct", import["instance"]!!.jsonPrimitive.content)
                    assertEquals("owner/repository", import["id"]!!.jsonPrimitive.content)
                    assertEquals("main", import["baseBranch"]!!.jsonPrimitive.content)
                    assertEquals(
                        "/fixtures/imported",
                        mutations
                            .last()
                            .second["settings"]!!
                            .jsonObject["path"]!!
                            .jsonPrimitive
                            .content,
                    )
                    assertEquals(
                        "remote",
                        mutations
                            .last()
                            .second["settings"]!!
                            .jsonObject["sourceMode"]!!
                            .jsonPrimitive
                            .content,
                    )
                }
                assertTrue(
                    mutations.first().second["creationId"]!!.jsonPrimitive.content.isNotBlank()
                )
                if (loseResponse) assertEquals(mutations[0].second, mutations[1].second)
            } finally {
                allowRecoveryProbe.countDown()
                allowRecoveryRefresh.countDown()
                models.clear()
                if (loseResponse) println(recoveryTimeline.joinToString("\n"))
            }
        }
    }

    private fun json(value: String) =
        MockResponse().setHeader("Content-Type", "application/json").setBody(value)
}
