package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
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
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
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
 * journey also renders review captures using Robolectric native graphics.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class FilUnreadJourneyTest {
    @get:Rule val compose = createComposeRule()

    private val now = System.currentTimeMillis()
    private val main =
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
            id = MAIN_AGENT_ID,
            name = "Rédacteur",
        )
    private val notes =
        Task(
            "348988cc-0279-5ed4-801b-ac44d6987c6e",
            "Notes de version hebdomadaires",
            "Résumer la semaine.",
            MAIN_AGENT_ID,
        )
    private val audit =
        Task(
            "b7ec3b50-62dd-530b-8060-bd5700fb7eb6",
            "Audit de sécurité nocturne",
            "Auditer.",
            MAIN_AGENT_ID,
        )
    private val activity =
        listOf(
            Run(
                "notes-run",
                taskId = "348988cc-0279-5ed4-801b-ac44d6987c6e",
                status = "succeeded",
                trigger = "schedule",
                startedAt = now - 30 * 60_000,
                finishedAt = now - 20 * 60_000,
                snapshot = Snapshot(task = notes.runTaskSnapshot(), agent = main),
                provider = "codex",
                unread = true,
                preview =
                    "Trois nouveautés cette semaine : reprise à la demande, partage d’installations et notifications.",
            ),
            Run(
                "audit-run",
                taskId = "b7ec3b50-62dd-530b-8060-bd5700fb7eb6",
                status = "failed",
                trigger = "schedule",
                startedAt = now - 7_200_000,
                finishedAt = now - 7_000_000,
                snapshot = Snapshot(task = audit.runTaskSnapshot(), agent = main),
                provider = "opencode",
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
                provider = "claude",
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
                provider = "claude",
            ),
            Chat(
                "old",
                "Relecture du guide d’installation",
                agentName = "Rédacteur",
                status = "succeeded",
                updatedAt = now - 86_400_000,
                finishedAt = now - 86_400_000,
                provider = "codex",
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
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(main))
                                    )
                                )
                            path == "/api/installations/fixture/api/tasks" ->
                                json(
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListMissionsOutput(
                                            listOf(notes, audit)
                                        )
                                    )
                                )
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
                            else ->
                                json(
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
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else "[]"
                                )
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
                // Conversations stream in before the missions' activity: wait for both.
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("2 résultats à lire", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.onNodeWithText("Le changelog est prêt", substring = true).assertExists()
                compose
                    .onNode(
                        hasText("Rédiger le changelog 0.53") and
                            hasContentDescription("Dernier harness : Claude Code")
                    )
                    .assertExists()
                capture("fil-unread-dark")
                theme = "light"
                capture("fil-unread-light")
                theme = "dark"
                compose
                    .onNodeWithTag("fil")
                    .performScrollToNode(hasText("Mission terminée", substring = true))
                compose.onNodeWithText("Trois nouveautés", substring = true).assertExists()
                compose
                    .onNode(
                        hasText(notes.name) and hasContentDescription("Dernier harness : Codex")
                    )
                    .assertExists()
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
                assertEquals(
                    "{\"items\":[{\"key\":\"chat:done\",\"finishedAt\":${chats.first().finishedAt}}]}",
                    reads.first(),
                )
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
                assertEquals(
                    "{\"items\":[{\"key\":\"task:348988cc-0279-5ed4-801b-ac44d6987c6e\",\"finishedAt\":${activity.first().finishedAt}}]}",
                    reads[1],
                )
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

    @Test
    fun `bulk read survives partial failure and resets for another account`() {
        val reads = Collections.synchronizedList(mutableListOf<String>())
        val account = java.util.concurrent.atomic.AtomicReference("fixture-account")
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val response =
                            when {
                                path == "/api/account/session" ->
                                    beaconAccountFixture("fixture")
                                        .replace("fixture-account", account.get())
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/reads") -> {
                                    reads += request.body.readUtf8()
                                    if (reads.size == 2)
                                        return MockResponse()
                                            .setResponseCode(503)
                                            .setBody("{\"error\":\"Offline\"}")
                                    "{\"read\":1}"
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
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
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(response)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme { androidx.compose.material3.Text("Read state test") }
            }
            fun waitFor(condition: () -> Boolean) =
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    condition()
                }
            try {
                waitFor { vm.state.value.installation != null && !vm.state.value.busy }
                compose.runOnIdle { vm.markRead((0..200).map { "chat:$it" to (now - 1000) }) }
                waitFor { reads.size == 2 && vm.state.value.error == "Offline" }
                val batches = reads.map {
                    wireJson.parseToJsonElement(it).jsonObject["items"]!!.jsonArray
                }
                assertEquals(listOf(200, 1), batches.map { it.size })
                compose.runOnIdle {
                    assertEquals(200, vm.localReads.size)
                    assertEquals(null, vm.localReads["chat:200"])
                    vm.report(ApiException(401, "Expired"))
                    assertEquals(0, vm.localReads.size)
                }
                account.set("member")
                compose.runOnIdle { vm.perform { connect(origin) } }
                waitFor { vm.state.value.session.account?.id == "member" && !vm.state.value.busy }
                compose.runOnIdle { vm.markRead(listOf("chat:0" to (now - 1000))) }
                waitFor { reads.size == 3 }
                compose.runOnIdle { assertEquals(now - 1000, vm.localReads["chat:0"]) }
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun `opening a conversation waits for its output before marking it read`() {
        val release = java.util.concurrent.CountDownLatch(1)
        val started = java.util.concurrent.atomic.AtomicBoolean(false)
        val reads = Collections.synchronizedList(mutableListOf<String>())
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/chats/done/stream")) {
                            started.set(true)
                            check(release.await(20, TimeUnit.SECONDS))
                            val run =
                                Run(
                                    "done-run",
                                    taskId = "done",
                                    status = "succeeded",
                                    trigger = "chat",
                                    finishedAt = chats.first().finishedAt,
                                )
                            return stream(LiveState(chat = chats.first().copy(run = run)))
                        }
                        if (path.endsWith("/chats/stream")) return stream(LiveState(chats = chats))
                        val response =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/reads") -> {
                                    reads += request.body.readUtf8()
                                    "{\"read\":1}"
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
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
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(response)
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.installation != null)
                        ChatScreen(vm, state, "done", openChat = {}, openRun = {})
                }
            }
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    started.get()
                }
                // Let the list arrive while the selected conversation response is still held.
                compose.waitUntil(10000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    compose
                        .onAllNodesWithTag("chat-page:question")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.waitForIdle()
                assertEquals(0, reads.size)
                release.countDown()
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    reads.isNotEmpty()
                }
                assertEquals(
                    "{\"items\":[{\"key\":\"chat:done\",\"finishedAt\":${chats.first().finishedAt}}]}",
                    reads.first(),
                )
            } finally {
                release.countDown()
                vm.api.closeStreams()
            }
        }
    }

    @Test
    fun `a mission finishing on files stays unread until its result is shown`() {
        val release = java.util.concurrent.CountDownLatch(1)
        val requests = java.util.concurrent.atomic.AtomicInteger()
        val reads = Collections.synchronizedList(mutableListOf<String>())
        val finished = activity.first().copy(summary = "Output waiting to be read")
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path.endsWith("/runs/notes-run/stream")) {
                            if (requests.getAndIncrement() == 0) {
                                val initial =
                                    LiveBatch(
                                        emptyList(),
                                        LiveState(
                                            run =
                                                finished.copy(status = "running", finishedAt = null)
                                        ),
                                        true,
                                        false,
                                    )
                                return MockResponse()
                                    .setHeader("Content-Type", "text/event-stream")
                                    .setBody(
                                        "event: batch\nid: 1\ndata: ${wireJson.encodeToString(initial)}\n\n"
                                    )
                            }
                            check(release.await(20, TimeUnit.SECONDS))
                            return stream(LiveState(run = finished))
                        }
                        val response =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/reads") -> {
                                    reads += request.body.readUtf8()
                                    "{\"read\":1}"
                                }
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
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
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/tasks") ==
                                            true
                                    )
                                        """{"tasks":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(response)
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.installation != null) RunScreen(vm, state, "notes-run", openRun = {})
                }
            }
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodesWithText("Fichiers").fetchSemanticsNodes().isNotEmpty()
                }
                compose.onNodeWithText("Fichiers").performClick()
                release.countDown()
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Terminée", substring = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                compose.waitForIdle()
                assertEquals(0, reads.size)
                compose.onNodeWithText("Résultat").performClick()
                compose.waitUntil(20000) {
                    compose.onAllNodes(isRoot()).fetchSemanticsNodes()
                    reads.isNotEmpty()
                }
                assertEquals(
                    "{\"items\":[{\"key\":\"task:348988cc-0279-5ed4-801b-ac44d6987c6e\",\"finishedAt\":${finished.finishedAt}}]}",
                    reads.single(),
                )
            } finally {
                release.countDown()
                vm.api.closeStreams()
            }
        }
    }
}
