package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
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
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class MissionAuthorityCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun newMissionCanOpenWithNoAgentAccessAndRecoversWhenAnAgentIsGranted() =
        withMissionEditor(emptyList(), waitForAccessBeforeOpen = true) { fixture ->
            fixture.waitFor {
                fixture.vm.state.value.usableAgents == emptyList<String>() &&
                    !fixture.vm.state.value.busy
            }
            fillMissionDraft()
            assertNoAgentAndNoWrite(fixture)
            fixture.grant(listOf(MAIN_AGENT_ID))
            savePreservedDraft(fixture)
        }

    @Test
    fun newMissionWaitsForAccessAndKeepsItsDraftWhenNoAgentIsGranted() =
        withMissionEditor(emptyList(), delayAccess = true) { fixture ->
            fixture.waitFor { fixture.accessRequested.count == 0L }
            compose.onNodeWithText("Chargement des agents…").assertExists()
            fillMissionDraft()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            fixture.allowAccess.countDown()
            fixture.waitFor {
                fixture.vm.state.value.usableAgents == emptyList<String>() &&
                    !fixture.vm.state.value.busy
            }
            assertNoAgentAndNoWrite(fixture)
            fixture.grant(listOf(MAIN_AGENT_ID))
            savePreservedDraft(fixture)
        }

    @Test
    fun revokingTheLastAgentWhileCreatingAMissionKeepsTheDraftThroughRestoreAndRegrant() =
        withMissionEditor(listOf(MAIN_AGENT_ID)) { fixture ->
            fixture.waitFor {
                fixture.vm.state.value.usableAgents == listOf(MAIN_AGENT_ID) &&
                    !fixture.vm.state.value.busy
            }
            fillMissionDraft()
            compose.onNodeWithText("Enregistrer").assertIsEnabled()
            fixture.grant(emptyList())
            assertNoAgentAndNoWrite(fixture)
            fixture.restoration.emulateSavedInstanceStateRestore()
            assertNoAgentAndNoWrite(fixture)
            fixture.grant(listOf(MAIN_AGENT_ID))
            savePreservedDraft(fixture)
        }

    @Test
    fun anExistingMissionKeepsItsDeniedAgentInsteadOfSilentlyMovingToAnAllowedAgent() =
        withMissionEditor(
            emptyList(),
            initial = Task(name = "Brouillon conservé", prompt = "Préparer la revue"),
        ) { fixture ->
            fixture.waitFor {
                fixture.vm.state.value.usableAgents == emptyList<String>() &&
                    !fixture.vm.state.value.busy
            }
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled()
            fixture.grant(listOf(otherAgentId))
            compose
                .onNodeWithText("Vous n’avez plus accès à Agent principal.", substring = true)
                .assertExists()
            compose.onNodeWithText("Enregistrer").assertIsNotEnabled().performClick()
            compose.runOnIdle { assertTrue(fixture.writes.isEmpty()) }
        }

    private fun fillMissionDraft() {
        compose.onNodeWithText("Nom").performTextInput("Brouillon conservé")
        compose
            .onNodeWithText("Mission et critères de réussite")
            .performTextInput("Préparer la revue")
    }

    private fun assertNoAgentAndNoWrite(fixture: MissionEditorFixture) {
        compose
            .onNodeWithText("Aucun agent ne vous est encore accessible.", substring = true)
            .assertExists()
        compose.onNodeWithText("Enregistrer").assertIsNotEnabled().performClick()
        compose.onNodeWithText("Brouillon conservé").assertExists()
        compose.onNodeWithText("Préparer la revue").assertExists()
        compose.runOnIdle { assertTrue(fixture.writes.isEmpty()) }
    }

    private fun savePreservedDraft(fixture: MissionEditorFixture) {
        compose.onNodeWithText("Brouillon conservé").assertExists()
        compose.onNodeWithText("Préparer la revue").assertExists()
        compose.onNodeWithText("Enregistrer").assertIsEnabled().performClick()
        fixture.waitFor { fixture.closed.get() }
        assertEquals(1, fixture.writes.size)
        val settings = fixture.writes.single()
        assertEquals(MAIN_AGENT_ID, settings["agentId"]?.jsonPrimitive?.content)
        assertEquals("Brouillon conservé", settings["name"]?.jsonPrimitive?.content)
        assertEquals("Préparer la revue", settings["prompt"]?.jsonPrimitive?.content)
    }

    private class MissionEditorFixture(
        val vm: CairnViewModel,
        val usable: AtomicReference<List<String>>,
        val writes: List<JsonObject>,
        val closed: AtomicBoolean,
        val accessRequested: CountDownLatch,
        val allowAccess: CountDownLatch,
        val restoration: StateRestorationTester,
        val waitFor: (() -> Boolean) -> Unit,
    ) {
        fun grant(agents: List<String>) {
            usable.set(agents)
            vm.perform { refreshUsableAgents() }
            waitFor { vm.state.value.usableAgents == agents && !vm.state.value.busy }
        }
    }

    private val otherAgentId = "d3153f9f-724b-59c3-9614-ab78f231c4d3"

    private fun withMissionEditor(
        initialUsable: List<String>,
        delayAccess: Boolean = false,
        waitForAccessBeforeOpen: Boolean = false,
        initial: Task? = null,
        check: (MissionEditorFixture) -> Unit,
    ) {
        val usable = AtomicReference(initialUsable)
        val writes = CopyOnWriteArrayList<JsonObject>()
        val closed = AtomicBoolean(false)
        val mounted = mutableStateOf(true)
        val accessRequested = CountDownLatch(1)
        val allowAccess = CountDownLatch(if (delayAccess) 1 else 0)
        val agents =
            listOf(MAIN_AGENT_ID to "Agent principal", otherAgentId to "Autre agent").map {
                (id, name) ->
                Agent(
                    id = id,
                    name = name,
                    revision = "fixture-revision",
                    createdAt = 0,
                    exclusiveNodes = emptyList(),
                    plugins = emptyList(),
                    access =
                        AccessPolicy(
                            mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                            projects = null,
                            skills = null,
                            mcps = null,
                        ),
                )
            }
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.requestUrl!!.encodedPath
                        val body =
                            when {
                                request.method in listOf("POST", "PUT") &&
                                    path.contains("/api/tasks") -> {
                                    val payload =
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    val settings = payload.getValue("settings").jsonObject
                                    writes += settings
                                    JsonObject(
                                            wireJson.encodeToJsonElement(Task()).jsonObject +
                                                settings
                                        )
                                        .toString()
                                }
                                path == "/api/account/session" ->
                                    beaconAccountFixture().replace("\"owner\"", "\"member\"")
                                path == "/api/installations" ->
                                    beaconInstallationsFixture().replace("\"owner\"", "\"member\"")
                                path.endsWith("/usable-agents") -> {
                                    accessRequested.countDown()
                                    kotlin.check(allowAccess.await(20, TimeUnit.SECONDS))
                                    """{"agentIds":${wireJson.encodeToJsonElement(usable.get())}}"""
                                }
                                path.endsWith("/api/agents") ->
                                    """{"agents":${wireJson.encodeToJsonElement(agents)}}"""
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/overview") -> installationOverviewFixture()
                                path.endsWith("/models") -> "{}"
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
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
            val origin = server.url("/").toString()
            val models = ViewModelStore().apply { put("mission-editor", vm) }
            val restoration = StateRestorationTester(compose)
            restoration.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (vm.state.value.installation == null) vm.connect(origin)
                }
                val state = vm.state.collectAsState().value
                if (
                    mounted.value &&
                        state.installation != null &&
                        !closed.get() &&
                        (!waitForAccessBeforeOpen || state.usableAgents != null)
                )
                    CairnTheme { TaskEditor(vm, state, initial, close = { closed.set(true) }) }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                check(
                    MissionEditorFixture(
                        vm,
                        usable,
                        writes,
                        closed,
                        accessRequested,
                        allowAccess,
                        restoration,
                        ::waitFor,
                    )
                )
            } finally {
                allowAccess.countDown()
                compose.runOnIdle { mounted.value = false }
                models.clear()
            }
        }
    }

    @Test
    fun anotherAuthorsCommitmentCanBeArchivedButNotEditedResumedRestoredOrDeleted() {
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("mission-authority", vm) }
        val task =
            mutableStateOf(
                Task(
                        name = "Engagement partagé",
                        prompt = "Travail approuvé",
                        cron = "0 9 * * *",
                        enabled = false,
                    )
                    .copy(authorId = "another-account")
            )
        val state =
            Workspace(
                installation = Installation("fixture", "Partagée", InstallationRole.Member),
                session = Session(account = CairnAccount("member", "member@example.test")),
            )
        val mounted = mutableStateOf(true)
        try {
            compose.setContent {
                if (mounted.value)
                    CairnTheme {
                        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                            MissionPanel(vm, state, task.value, {}, {})
                        }
                    }
            }
            compose
                .onNodeWithContentDescription("Modifier la mission")
                .performScrollTo()
                .assertIsNotEnabled()
            compose.onNodeWithContentDescription("Reprendre la planification").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Autres actions").performScrollTo().performClick()
            compose.onNodeWithText("Archiver").assertIsEnabled()
            compose.onNodeWithText("Supprimer").assertDoesNotExist()
            compose.onNodeWithText("Dupliquer (en pause)").assertIsEnabled()
            compose.runOnIdle { task.value = task.value.copy(archived = true) }
            compose.onNodeWithText("Restaurer (en pause)").assertIsNotEnabled()
        } finally {
            compose.runOnIdle { mounted.value = false }
            models.clear()
        }
    }
}
