package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import java.util.Collections
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Finished work waits in Pour vous until it is read, then leaves it for every device. The same
 * journey runs on a device in `FilUnreadDeviceTest`; this one also renders review captures.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class FilUnreadJourneyTest {
    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val main = Agent(MAIN_AGENT_ID, "Rédacteur")
    private val notes =
        Task("notes", "Notes de version hebdomadaires", "Résumer la semaine.", MAIN_AGENT_ID)
    private val audit = Task("audit", "Audit de sécurité nocturne", "Auditer.", MAIN_AGENT_ID)
    private val activity =
        listOf(
            Run(
                "notes-run",
                taskId = "notes",
                status = "succeeded",
                trigger = "schedule",
                startedAt = now - 30 * 60_000,
                finishedAt = now - 20 * 60_000,
                snapshot = Snapshot(task = notes, agent = main),
                unread = true,
                preview =
                    "Trois nouveautés cette semaine : reprise à la demande, partage d’installations et notifications.",
            ),
            Run(
                "audit-run",
                taskId = "audit",
                status = "failed",
                trigger = "schedule",
                startedAt = now - 7_200_000,
                finishedAt = now - 7_000_000,
                snapshot = Snapshot(task = audit, agent = main),
            ),
        )
    private val chats =
        listOf(
            Chat(
                "done",
                "Rédiger le changelog 0.53",
                agentName = "Rédacteur",
                status = "succeeded",
                runId = "done-run",
                updatedAt = now - 4 * 60_000,
                finishedAt = now - 3 * 60_000,
                unread = true,
                preview =
                    "Le changelog est prêt : 12 corrections, 3 nouveautés. J’ai ouvert la PR #184.",
            ),
            Chat(
                "question",
                "Choisir la pagination",
                agentName = "Rédacteur",
                pendingQuestions = 1,
                updatedAt = now - 60_000,
            ),
            Chat(
                "old",
                "Relecture du guide d’installation",
                agentName = "Rédacteur",
                status = "succeeded",
                updatedAt = now - 86_400_000,
                finishedAt = now - 86_400_000,
            ),
        )

    private fun stream(state: LiveState): MockResponse {
        val frame =
            "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    @org.junit.Before
    fun prepare() {
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @org.junit.After
    fun finish() = androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()

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
    fun `finished work waits in Pour vous until it is read`() {
        val reads = Collections.synchronizedList(mutableListOf<String>())
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val json = { body: String ->
                            MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(body)
                        }
                        return when {
                            path == "/api/installations" -> json(beaconInstallationsFixture())
                            path == "/api/account/session" -> json(beaconAccountFixture("fixture"))
                            path == "/api/installations/fixture/api/agents" ->
                                json(wireJson.encodeToString(listOf(main)))
                            path == "/api/installations/fixture/api/tasks" ->
                                json(wireJson.encodeToString(listOf(notes, audit)))
                            path == "/api/installations/fixture/api/tasks/activity" ->
                                json(wireJson.encodeToString(activity))
                            path == "/api/installations/fixture/api/chats/stream" ->
                                stream(LiveState(chats = chats))
                            path == "/api/installations/fixture/api/reads" -> {
                                reads += request.body.readUtf8()
                                json("{\"read\":1}")
                            }
                            path == "/api/installations/fixture/api/overview" ||
                                path.endsWith("/models") ||
                                path == "/api/installations/fixture/api/accounts" -> json("{}")
                            else -> json("[]")
                        }
                    }
                }
            val origin = server.url("/").toString()
            val app = ApplicationProvider.getApplicationContext<Application>()

            val vm = CairnViewModel(app, MemoryVault(), beaconOrigin = "")
            var theme by mutableStateOf("dark")
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme(theme) { CairnApp(vm = vm) }
            }
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodesWithTag("unread").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("2 résultats à lire", substring = true).assertExists()
                compose.onNodeWithText("Le changelog est prêt", substring = true).assertExists()
                capture("fil-unread-dark")
                theme = "light"
                capture("fil-unread-light")
                theme = "dark"
                compose
                    .onNodeWithTag("fil")
                    .performScrollToNode(hasText("Mission terminée", substring = true))
                compose.onNodeWithText("Trois nouveautés", substring = true).assertExists()
                capture("fil-unread-dark-scrolled")
                compose.onNodeWithTag("fil").performScrollToIndex(0)

                compose
                    .onNodeWithContentDescription("Marquer Rédiger le changelog 0.53 comme lu")
                    .performClick()
                compose.waitUntil(10000) {
                    // Reading the tree runs the main looper, where the request resumes.
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    reads.isNotEmpty()
                }
                assertEquals("{\"items\":[\"chat:done\"]}", reads.first())
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithContentDescription(
                            "Marquer Rédiger le changelog 0.53 comme lu"
                        )
                        .fetchSemanticsNodes()
                        .isEmpty()
                }
                compose.onNodeWithText("1 résultat à lire", substring = true).assertExists()

                compose.onNodeWithTag("read-all").performClick()
                compose.waitUntil(10000) {
                    // Reading the tree runs the main looper, where the request resumes.
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    reads.size == 2
                }
                assertEquals("{\"items\":[\"task:notes\"]}", reads[1])
                compose.waitUntil(10000) {
                    compose.onAllNodesWithTag("read-all").fetchSemanticsNodes().isEmpty()
                }
                compose.onAllNodesWithTag("unread").assertCountEquals(0)
                capture("fil-read-dark")
            } finally {
                vm.api.closeStreams()
            }
        }
    }
}
