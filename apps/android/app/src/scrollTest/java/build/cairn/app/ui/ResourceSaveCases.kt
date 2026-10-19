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

/** Native editors must retain the committed identity through refresh and response failures. */
abstract class ResourceSaveCases {
    @get:Rule val compose = createComposeRule()

    protected open fun captureFailure(name: String) = Unit

    @Test fun taskRefreshFailureKeepsTheCreatedMission() = saveAfterFailure("tasks", false)

    @Test fun projectRefreshFailureKeepsTheCreatedProject() = saveAfterFailure("projects", false)

    @Test
    fun githubImportRefreshFailureKeepsTheImportedProject() = saveAfterFailure("github", false)

    @Test
    fun lostGithubImportResponseRecoversTheRepositoryBeforeUpdatingEdits() =
        saveAfterFailure("github", true)

    @Test fun agentRefreshFailureKeepsTheCreatedAgent() = saveAfterFailure("agents", false)

    @Test
    fun lostTaskResponseReplaysTheOriginalCreationThenUpdatesTheEditedMission() =
        saveAfterFailure("tasks", true)

    @Test
    fun lostProjectResponseReplaysTheOriginalCreationThenUpdatesTheEditedProject() =
        saveAfterFailure("projects", true)

    @Test
    fun lostAgentResponseReplaysTheOriginalCreationThenUpdatesTheEditedAgent() =
        saveAfterFailure("agents", true)

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
    fun dedicatedTokenFailureDoesNotCreateAnotherAgent() =
        saveAfterFailure("agents", false, tokenFailure = true)

    @Test
    fun restoredTaskEditorKeepsItsCommittedIdentity() =
        saveAfterFailure("tasks", false, restore = true)

    @Test
    fun restoredTaskEditorReplaysItsUncertainCreation() =
        saveAfterFailure("tasks", true, restore = true)

    private fun saveAfterFailure(
        kind: String,
        loseResponse: Boolean,
        rejectReplay: Boolean = false,
        tokenFailure: Boolean = false,
        restore: Boolean = false,
        malformedResponse: Boolean = false,
    ) {
        val uncertainResponse = loseResponse || malformedResponse
        val resourceKind = if (kind == "github") "projects" else kind
        val mutations = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        val failRefresh = AtomicBoolean(false)
        val closed = AtomicBoolean(false)
        val created = AtomicBoolean(false)
        var committed: JsonObject? = null
        val failedToken = AtomicBoolean(false)
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
                        if (path.endsWith("/github-token") && request.method == "PUT") {
                            if (tokenFailure && !failedToken.getAndSet(true))
                                return MockResponse()
                                    .setResponseCode(400)
                                    .setBody("""{"error":"Jeton refusé"}""")
                            return json("{}")
                        }
                        if (
                            request.method in listOf("POST", "PUT") &&
                                (path == resourcePath ||
                                    path == "$resourcePath/saved" ||
                                    path == "$resourcePath/github")
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
                            if (first && !tokenFailure) failRefresh.set(true)
                            else failRefresh.set(false)
                            val stored = if (request.method == "POST") committed!! else value
                            val projectPath =
                                if (kind == "github")
                                    mapOf("path" to JsonPrimitive("/fixtures/imported"))
                                else emptyMap()
                            return json(
                                JsonObject(stored + projectPath + ("id" to JsonPrimitive("saved")))
                                    .toString()
                            )
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
                                path.endsWith("/github/repositories") ->
                                    """{"repositories":[{"id":1,"name":"repository","fullName":"owner/repository","defaultBranch":"main"}]}"""
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                path.endsWith("/agents") -> """[{"id":"agent","name":"Agent"}]"""
                                else -> "[]"
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
                            "github" ->
                                ProjectEditor(
                                    vm,
                                    state,
                                    Project(
                                        name = "Original",
                                        path = "/fixtures/project",
                                        sourceMode = if (kind == "github") "local" else "remote",
                                    ),
                                    close = { closed.set(true) },
                                )
                            else ->
                                AgentEditor(
                                    vm,
                                    state,
                                    Agent(
                                        name = "Original",
                                        access = AccessPolicy(github = !tokenFailure),
                                    ),
                                    close = { closed.set(true) },
                                )
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
                if (kind == "github") {
                    compose.onNodeWithText("Dossier sur le serveur").performClick()
                    compose.onNodeWithText("GitHub").performClick()
                    compose.waitUntil(20000) {
                        compose
                            .onAllNodesWithText("owner/repository")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("owner/repository").performScrollTo().performClick()
                }
                if (tokenFailure)
                    compose
                        .onNodeWithText("Jeton GitHub dédié (facultatif)")
                        .performScrollTo()
                        .performTextInput("fixture-token")
                compose
                    .onNode(hasScrollAction() and hasAnyDescendant(hasText("Nom")))
                    .performTouchInput { swipeUp() }
                compose.onNodeWithText("Enregistrer").performClick()
                waitFor { mutations.isNotEmpty() && !vm.state.value.busy }
                assertFalse(closed.get())
                if (tokenFailure) {
                    compose
                        .onNodeWithText(
                            "Agent enregistré, mais le jeton GitHub n’a pas été enregistré. Réessayez."
                        )
                        .assertIsDisplayed()
                } else if (!uncertainResponse) {
                    val label =
                        when (kind) {
                            "tasks" -> "Mission"
                            "projects",
                            "github" -> "Projet"
                            else -> "Agent"
                        }
                    compose
                        .onNodeWithText(
                            "$label enregistré${if (kind == "tasks") "e" else ""}, mais le rechargement a échoué. Réessayez."
                        )
                        .assertIsDisplayed()
                }
                if (uncertainResponse)
                    compose.onNodeWithText("Sauvegarde", substring = true).assertIsDisplayed()
                if (kind == "agents" && !uncertainResponse)
                    compose
                        .onAllNodesWithText("Enregistrez l’agent pour importer un portrait.")
                        .assertCountEquals(0)
                captureFailure(
                    "$kind-${if (tokenFailure) "token" else if (uncertainResponse) "uncertain" else "reload"}"
                )
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
                assertEquals("Modifié", mutations.last().second["name"]!!.jsonPrimitive.content)
                if (kind == "github") {
                    assertEquals(
                        "/fixtures/imported",
                        mutations.last().second["path"]!!.jsonPrimitive.content,
                    )
                    assertEquals(
                        "remote",
                        mutations.last().second["sourceMode"]!!.jsonPrimitive.content,
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
