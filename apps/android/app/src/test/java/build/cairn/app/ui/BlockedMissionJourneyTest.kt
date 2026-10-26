package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Blocked missions in the Fil: only those waiting on the user ask in Pour vous, and each can be set
 * aside from its card or its run.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class BlockedMissionJourneyTest {
    @get:Rule val compose = createComposeRule()

    private val calls = CopyOnWriteArrayList<Triple<String, String, String>>()
    private val activityReads = CopyOnWriteArrayList<List<Run>>()
    private val savedMissions = ConcurrentHashMap<String, Task>()
    private val now = System.currentTimeMillis()
    private val claude =
        Agent(
            exclusiveNodes = emptyList(),
            plugins = emptyList(),
            revision = "",
            createdAt = 0,
            access =
                AccessPolicy(
                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                    projects = null,
                    skills = null,
                    mcps = null,
                ),
            id = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
            name = "Claude",
            provider = "claude",
        )
    private val proof =
        Task(
            "2db836ee-ce2e-552d-b9fa-2fef2030546c",
            "Preuve appareil F8",
            agentId = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
        )
    private val notes =
        Task(
            "348988cc-0279-5ed4-801b-ac44d6987c6e",
            "Notes de version 0.53",
            agentId = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
        )
    private val checks =
        Task(
            "25078a9c-7f0c-5a2b-8424-e17e90946633",
            "Checks de la PR315",
            agentId = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
        )
    private val deploy =
        Task(
            "888f7003-e15b-5b1f-a1b3-a221bc92ec3a",
            "Déploiement staging",
            agentId = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
        )
    private val parent =
        Chat(
            "parent",
            "Orchestrer les chantiers F8",
            agentId = "8630fb2d-e8d4-5aac-9c89-6874bb9a60b1",
            agentName = "Claude",
            updatedAt = now - 3_600_000,
        )

    private fun run(
        task: Task,
        outcome: TaskOutcome,
        minutes: Long,
        requestedBy: RunRequester? = null,
    ) =
        Run(
            "${task.id}-run",
            taskId = task.id,
            status = "succeeded",
            trigger = "mcp",
            createdAt = now - minutes * 60_000 - 300_000,
            startedAt = now - minutes * 60_000 - 300_000,
            finishedAt = now - minutes * 60_000,
            snapshot = Snapshot(task = task.runTaskSnapshot(), agent = claude),
            outcome = outcome.copy(reportedAt = now - minutes * 60_000),
            requestedBy = requestedBy,
        )

    private val runs =
        listOf(
            run(
                proof,
                TaskOutcome(
                    "blocked",
                    "La preuve appareil est faite ; la livraison attend la CI et le merge.",
                ),
                5,
            ),
            run(
                notes,
                TaskOutcome("needs_input", "Choisir si les notes couvrent la version Android."),
                12,
            ),
            run(
                checks,
                TaskOutcome(
                    "blocked",
                    "Le parent fusionne une fois la CI verte, comme demandé.",
                    waitingOn = "requester",
                ),
                20,
                RunRequester("parent-run", chatId = "parent"),
            ),
            run(
                deploy,
                TaskOutcome(
                    "blocked",
                    "Le déploiement attend la fin de la maintenance de l’hébergeur.",
                    waitingOn = "external",
                ),
                30,
            ),
        )

    /** What this account set aside, as the server keeps it. */
    private val dismissals = ConcurrentHashMap<String, OutcomeDismissal>()

    @org.junit.Before
    fun prepare() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @org.junit.After
    fun finish() = androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()

    private fun stream(state: LiveState): MockResponse {
        val frame =
            "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    private fun json(body: String) =
        MockResponse().setHeader("Content-Type", "application/json").setBody(body)

    private fun activity() = runs.map { run ->
        dismissals[run.taskId]?.let { run.copy(dismissal = it) } ?: run
    }

    private fun setAside(body: String) {
        val item = wireJson.parseToJsonElement(body).jsonObject["items"]!!.jsonArray.single()
        val task = item.jsonObject["key"]!!.jsonPrimitive.content.removePrefix("task:")
        if (item.jsonObject["restore"]?.jsonPrimitive?.boolean == true) dismissals.remove(task)
        else
            dismissals[task] = OutcomeDismissal(item.jsonObject["until"]?.jsonPrimitive?.longOrNull)
    }

    private fun journey(body: (CairnViewModel) -> Unit) {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val text = request.body.readUtf8()
                        calls += Triple(request.method!!, request.path!!, text)
                        val api = "/api/installations/fixture/api"
                        return when {
                            path == "/api/installations" -> json(beaconInstallationsFixture())
                            path == "/api/account/session" -> json(beaconAccountFixture("fixture"))
                            path == "$api/agents" ->
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(claude))
                                    )
                                )
                            path == "$api/tasks" && request.method == "GET" ->
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListMissionsOutput(
                                            listOf(proof, notes, checks, deploy).map {
                                                savedMissions[it.id] ?: it
                                            }
                                        )
                                    )
                                )
                            path.startsWith("$api/tasks/") &&
                                path.endsWith("/state") &&
                                request.method == "POST" -> {
                                val command =
                                    wireJson.decodeFromString<
                                        build.cairn.contracts.MissionStateInput
                                    >(
                                        text
                                    )
                                val current =
                                    listOf(proof, notes, checks, deploy).single {
                                        it.id == command.taskId
                                    }
                                val saved =
                                    current.copy(
                                        enabled = command.enabled,
                                        archived = command.archived,
                                        revision = "saved-state",
                                    )
                                savedMissions[current.id] = saved
                                json(wireJson.encodeToString(saved))
                            }
                            path == "$api/tasks/activity" -> {
                                val snapshot = activity()
                                activityReads += snapshot
                                json(wireJson.encodeToString(snapshot))
                            }
                            path == "$api/dismissals" -> {
                                setAside(text)
                                json("{\"updated\":1}")
                            }
                            path == "$api/chats/stream" -> stream(LiveState(chats = listOf(parent)))
                            path.startsWith("$api/runs/") && path.endsWith("/stream") -> {
                                val id = path.removePrefix("$api/runs/").removeSuffix("/stream")
                                stream(LiveState(run = activity().first { it.id == id }))
                            }
                            path == "$api/accounts" -> json("{\"accounts\":[],\"required\":[]}")
                            path == "$api/projects" -> json("""{"projects":[]}""")
                            path == "$api/skills" ||
                                path == "$api/mcps" ||
                                path == "$api/onepassword" ||
                                path == "$api/runs" -> json("[]")
                            else -> json("{}")
                        }
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme("light") { CairnApp(vm = vm) }
            }
            try {
                body(vm)
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun waitText(value: String, substring: Boolean = false) =
        compose.waitUntil(15000) {
            compose
                .onAllNodesWithText(value, substring = substring)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }

    private fun waitCall(method: String, path: String) =
        compose.waitUntil(15000) {
            compose.onAllNodes(isRoot()).fetchSemanticsNodes()
            calls.any { it.first == method && it.second.startsWith(path) }
        }

    private fun capture(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        compose
            .onRoot()
            .captureToImage()
            .asAndroidBitmap()
            .compress(
                android.graphics.Bitmap.CompressFormat.PNG,
                100,
                File(dir, "$name.png").outputStream(),
            )
    }

    @Test
    fun `only blocked missions waiting on the user ask, and each can be set aside from the Fil`() =
        journey {
            waitText("Preuve appareil F8")
            waitText("Checks de la PR315")
            // What waits on the conversation that started it, or on an outside check, never asks.
            waitText("Tâche bloquée · En attente de Orchestrer les chantiers F8")
            waitText("Tâche bloquée · En attente d’une vérification externe")
            waitText("2 éléments pour vous", substring = true)
            compose.onNodeWithContentDescription("Écarter Preuve appareil F8").assertExists()
            // Input the agent asked for can only be snoozed.
            compose
                .onAllNodesWithContentDescription("Écarter Notes de version 0.53")
                .assertCountEquals(0)
            capture("android-blocked-fil")

            compose
                .onNodeWithContentDescription("Autres actions pour Notes de version 0.53")
                .performClick()
            waitText("Me le rappeler demain")
            compose.onNodeWithText("Relancer").assertExists()
            compose.onNodeWithText("Archiver la mission").assertExists()
            compose.onNodeWithText("Me le rappeler demain").performClick()
            waitCall("POST", "/api/installations/fixture/api/dismissals")
            waitText("Rappel prévu · Votre réponse est nécessaire")
            val snooze =
                wireJson
                    .parseToJsonElement(calls.last { it.second.endsWith("/dismissals") }.third)
                    .jsonObject["items"]!!
                    .jsonArray
                    .single()
                    .jsonObject
            assertEquals(
                "task:348988cc-0279-5ed4-801b-ac44d6987c6e",
                snooze["key"]!!.jsonPrimitive.content,
            )
            assertEquals(tomorrowMorning(), snooze["until"]!!.jsonPrimitive.long)

            compose.onNodeWithContentDescription("Écarter Preuve appareil F8").performClick()
            waitText("Tâche bloquée · Écartée")
            assertTrue(dismissals.containsKey("2db836ee-ce2e-552d-b9fa-2fef2030546c"))
            capture("android-blocked-set-aside")
        }

    @Test
    fun `archiving from the Fil uses the current mission revision`() = journey { vm ->
        waitText("Preuve appareil F8")
        compose
            .onNodeWithContentDescription("Autres actions pour Preuve appareil F8")
            .performScrollTo()
            .performClick()
        waitText("Archiver la mission")
        compose.onNodeWithText("Archiver la mission").performClick()
        val statePath = "/api/installations/fixture/api/tasks/${proof.id}/state"
        waitCall("POST", statePath)
        val command =
            wireJson.decodeFromString<build.cairn.contracts.MissionStateInput>(
                calls.last { it.second == statePath }.third
            )
        assertEquals(proof.id, command.taskId)
        assertEquals(proof.revision, command.revision)
        assertEquals(false, command.enabled)
        assertEquals(true, command.archived)
        waitText("Modifications enregistrées")
        assertEquals("Archive refresh failed", null, vm.state.value.error)
        assertTrue(vm.state.value.tasks.any { it.id == proof.id && it.archived && !it.enabled })
    }

    @Test
    fun `a blocked run says who it waits on and can be set aside from its outcome`() = journey {
        waitText("Preuve appareil F8")
        compose.onNodeWithText("Preuve appareil F8").performClick()
        waitText("Me le rappeler demain")
        compose.onNodeWithText("Écarter").assertExists()
        capture("android-blocked-run-actions")
        compose.onNodeWithText("Écarter").performClick()
        waitText("Écartée : elle reste hors de Pour vous jusqu’au prochain résultat.")
        waitText("Remettre dans Pour vous")
        capture("android-blocked-run-dismissed")
        compose.onNodeWithText("Remettre dans Pour vous").performClick()
        waitText("Me le rappeler demain")
        assertTrue(!dismissals.containsKey("2db836ee-ce2e-552d-b9fa-2fef2030546c"))
    }

    @Test
    fun `server changes from another device remain visible after local dismiss and restore`() =
        journey {
            waitText("Preuve appareil F8")
            compose.onNodeWithContentDescription("Écarter Preuve appareil F8").performClick()
            waitText("Tâche bloquée · Écartée")
            compose.waitUntil(15000) {
                activityReads.any { snapshot ->
                    snapshot.first { it.taskId == proof.id }.dismissal != null
                }
            }

            // The same account restores the outcome from another device.
            dismissals.remove(proof.id)
            compose.waitUntil(15000) {
                compose
                    .onAllNodesWithContentDescription("Écarter Preuve appareil F8")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }

            dismissals[proof.id] = OutcomeDismissal(tomorrowMorning())
            waitText("Rappel prévu · Tâche bloquée")
            compose.onNodeWithText("Preuve appareil F8").performClick()
            waitText("Remettre dans Pour vous")
            val beforeRestore = activityReads.size
            compose.onNodeWithText("Remettre dans Pour vous").performClick()
            waitText("Me le rappeler demain")
            compose.waitUntil(15000) {
                activityReads.drop(beforeRestore).any { snapshot ->
                    snapshot.first { it.taskId == proof.id }.dismissal == null
                }
            }

            // A later dismissal elsewhere supersedes Android's local restore too.
            dismissals[proof.id] = OutcomeDismissal()
            waitText("Écartée : elle reste hors de Pour vous jusqu’au prochain résultat.")
        }

    @Test
    fun `a run handed back to its requester links the conversation that started it`() = journey {
        waitText("Checks de la PR315")
        compose.onNodeWithText("Checks de la PR315").performClick()
        waitText("En attente de « Orchestrer les chantiers F8 »", substring = true)
        waitText("Ouvrir « Orchestrer les chantiers F8 »")
        capture("android-blocked-run-requester")
    }
}
