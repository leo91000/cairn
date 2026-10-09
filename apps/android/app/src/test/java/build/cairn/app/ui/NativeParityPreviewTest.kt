package build.cairn.app.ui

import android.app.Application
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/** Native Android rendering previews; these are explicitly not emulator captures. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NativeParityPreviewTest {
    @get:Rule val compose = createComposeRule()

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
    fun finish() {
        androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test fun phoneConversationEvidenceAndTaskLayout() = preview(false)

    @Test
    @Config(qualifiers = "w1280dp-h800dp-mdpi")
    fun tabletTaskInboxAndConversationLayout() = preview(true)

    @Test
    @Config(qualifiers = "w360dp-h800dp-mdpi")
    fun filFilesAndComposerStayCompactOnSmallPhone() = preview(false, compact = true)

    @Test
    @Config(qualifiers = "w360dp-h800dp-mdpi")
    fun filRemainsReadableWithEnlargedText() = preview(false, compact = true, largeText = true)

    private fun preview(wide: Boolean, compact: Boolean = false, largeText: Boolean = false) {
        MockWebServer().use { server ->
            val now = System.currentTimeMillis()
            val task =
                Task(
                    "task",
                    "Revue hebdomadaire du projet",
                    "Vérifier les changements et préparer le compte rendu.",
                    MAIN_AGENT_ID,
                    cron = "0 9 * * 1",
                    nextRun = now + 86400000,
                )
            val agent = Agent(MAIN_AGENT_ID, "Agent principal")
            val project = Project("project", "Cairn")
            val outcome =
                TaskOutcome(
                    "completed",
                    "Les changements sont prêts.",
                    listOf(
                        "Vérifications réussies et compte rendu préparé.",
                        "La navigation et les messages longs ont été vérifiés.",
                    ),
                    now,
                    "reply",
                )
            val run =
                Run(
                    "run",
                    taskId = task.id,
                    status = "succeeded",
                    taskName = task.name,
                    snapshot = Snapshot(task = task, agent = agent, project = project),
                    outcome = outcome,
                )
            val chat =
                Chat(
                    "chat",
                    "Simplifier l’interface mobile",
                    agentName = agent.name,
                    projectName = project.name,
                    runId = run.id,
                    run = run.copy(trigger = "chat"),
                    updatedAt = now,
                )
            val files =
                if (compact)
                    listOf(
                        Deliverable(
                            "report",
                            "run",
                            key = "report",
                            messageId = "reply",
                            title = "Compte rendu de la version",
                            name = "rapport.md",
                            kind = "markdown",
                            mediaType = "text/markdown",
                            size = 12288,
                            createdAt = now,
                            group = "Rapports",
                        ),
                        Deliverable(
                            "notes",
                            "run",
                            key = "notes",
                            messageId = "reply",
                            title = "Notes de validation",
                            name = "validation.md",
                            kind = "markdown",
                            mediaType = "text/markdown",
                            size = 8192,
                            createdAt = now,
                            group = "Rapports",
                        ),
                        Deliverable(
                            "changes",
                            "run",
                            key = "changes",
                            messageId = "reply",
                            title = "Détails des modifications",
                            name = "modifications.md",
                            kind = "markdown",
                            mediaType = "text/markdown",
                            size = 4096,
                            createdAt = now,
                            group = "Rapports",
                        ),
                    )
                else emptyList()
            val reply =
                if (compact)
                    "La nouvelle interface est prête. **La lecture est plus légère** : les messages gardent leur place et les fichiers prennent moins de hauteur.\n\n" +
                        "Les détails restent accessibles au toucher, sans encombrer la conversation."
                else
                    "La nouvelle interface est prête.\n\n- Navigation compacte et explicite\n- Détails disponibles à la demande\n- Plus de place pour la conversation\n\nLes longues adresses restent lisibles : https://example.test/" +
                        "une-longue-adresse-".repeat(7)
            val events =
                listOf(
                    RunEvent(1, now - 60000, "chat.user", "Peux-tu simplifier cette interface ?"),
                    RunEvent(
                        2,
                        now,
                        "item.completed",
                        "",
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("id", "reply")
                                    put("type", "agent_message")
                                    put("text", reply)
                                }
                        ),
                    ),
                )
            fun stream(state: LiveState): MockResponse {
                val frame =
                    "event: batch\nid: 2\ndata: ${wireJson.encodeToString(LiveBatch(events, state.copy(artifacts = files), true, false))}\n\n"
                return MockResponse()
                    .setHeader("Content-Type", "text/event-stream")
                    .setBody(frame + ": keepalive\n\n".repeat(100000))
                    .throttleBody(
                        frame.toByteArray().size.toLong(),
                        1,
                        java.util.concurrent.TimeUnit.SECONDS,
                    )
            }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        if (
                            request.path!!.startsWith(
                                "/api/installations/fixture/api/runs/run/artifacts/"
                            )
                        )
                            return MockResponse()
                                .setHeader("Content-Type", "text/markdown")
                                .setBody("# Compte rendu\n\nLa revue est terminée.")
                        val body =
                            when (request.path!!.substringBefore('?')) {
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent("custom", "Agent spécialisé"), agent)
                                    )
                                "/api/installations/fixture/api/projects" ->
                                    wireJson.encodeToString(listOf(project))
                                "/api/installations/fixture/api/tasks" ->
                                    wireJson.encodeToString(
                                        listOf(
                                            task,
                                            task.copy(
                                                id = "next",
                                                name = "Préparer la prochaine version",
                                                cron = null,
                                                nextRun = null,
                                            ),
                                        )
                                    )
                                "/api/installations/fixture/api/tasks/activity" ->
                                    wireJson.encodeToString(listOf(run))
                                "/api/installations/fixture/api/runs" ->
                                    wireJson.encodeToString(listOf(run))
                                "/api/installations/fixture/api/schedule/preview" ->
                                    """{"occurrences":[${now + 86400000},${now + 8 * 86400000}]}"""
                                "/api/installations/fixture/api/chats/stream" ->
                                    return stream(LiveState(chats = listOf(chat)))
                                "/api/installations/fixture/api/chats/chat/stream" ->
                                    return stream(LiveState(chat = chat, run = chat.run))
                                "/api/installations/fixture/api/runs/run/stream" ->
                                    return stream(LiveState(run = run))
                                "/api/installations/fixture/api/skills",
                                "/api/installations/fixture/api/mcps" -> "[]"
                                else -> "{}"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                    beaconOrigin = "",
                )
            lateinit var activity: ComponentActivity
            compose.setContent {
                activity = LocalActivity.current as ComponentActivity
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                val density = LocalDensity.current
                CompositionLocalProvider(
                    LocalDensity provides
                        Density(density.density, if (largeText) 1.3f else density.fontScale)
                ) {
                    CairnTheme("dark") { CairnApp(vm = vm) }
                }
            }
            try {
                waitText(chat.title)
                if (!wide) {
                    compose.onNodeWithText(chat.title).performClick()
                    waitText("Tâche terminée")
                    compose
                        .onNodeWithTag("conversation-history")
                        .performScrollToNode(hasText("Tâche terminée"))
                    awaitMarkdown(activity, "La nouvelle interface")
                    if (compact) {
                        val composer = compose.onNodeWithTag("conversation-composer")
                        // Message field plus one toolbar holding attach, agent/model/effort and
                        // send.
                        composer
                            .assertIsDisplayed()
                            .assertCompactHeight(if (largeText) 136.dp else 112.dp)
                        compose
                            .onNodeWithTag("model-picker")
                            .assertIsDisplayed()
                            .assert(hasContentDescription("Codex", substring = true))
                        compose
                            .onNodeWithTag("conversation-header")
                            .assertCompactHeight(if (largeText) 80.dp else 64.dp)
                        compose
                            .onNodeWithTag("conversation-files")
                            .assertIsDisplayed()
                            .assertCompactHeight(if (largeText) 116.dp else 88.dp)
                        val sendBounds =
                            compose.onNodeWithTag("conversation-send").getUnclippedBoundsInRoot()
                        org.junit.Assert.assertEquals(48.dp, sendBounds.bottom - sendBounds.top)
                        org.junit.Assert.assertEquals(48.dp, sendBounds.right - sendBounds.left)
                        val faceBounds =
                            compose
                                .onNodeWithTag("conversation-send-face", useUnmergedTree = true)
                                .getUnclippedBoundsInRoot()
                        org.junit.Assert.assertEquals(40.dp, faceBounds.bottom - faceBounds.top)
                        org.junit.Assert.assertEquals(40.dp, faceBounds.right - faceBounds.left)
                        val name = if (largeText) "fil-phone-large-text" else "fil-phone-density"
                        capture(name)
                        // Compact presentation must retain access to every file and the native
                        // viewer.
                        compose.onNodeWithTag("conversation-files").performScrollToIndex(2)
                        compose
                            .onNodeWithText("Détails des modifications")
                            .assertIsDisplayed()
                            .performClick()
                        waitText("modifications.md")
                        compose.onNodeWithText("Version 1").assertExists()
                        return
                    }
                    capture("chat-phone-dark")
                    compose.onNodeWithText("Détails").performClick()
                    compose.onNode(hasText("Rapporté par", substring = true)).performScrollTo()
                    capture("chat-evidence-phone-dark")
                    compose.runOnIdle { activity.onBackPressedDispatcher.onBackPressed() }
                    waitDescription("Missions")
                }
                // Phone dock tabs are named by their icon; the large-window rail shows its labels.
                compose
                    .onAllNodes(
                        (hasContentDescription("Missions") or hasText("Missions")) and
                            hasClickAction()
                    )
                    .onFirst()
                    .performClick()
                waitText(task.name)
                // Missions state their schedule in words and show recent run history.
                compose.onNodeWithText("Chaque lundi · 09:00", substring = true).assertExists()
                waitDescription("réussies", substring = true)
                capture(if (wide) "missions-tablet-dark" else "missions-phone-dark")
                if (wide) {
                    // The detail pane shows the first mission next to the list.
                    compose.onNodeWithText("09:00").assertIsDisplayed()
                    compose.onNodeWithText("Lancer maintenant").assertIsDisplayed()
                } else {
                    compose.onNodeWithText(task.name).performClick()
                    compose.onNodeWithTag("mission-sheet").assertIsDisplayed()
                    compose.waitUntil(15000) {
                        compose
                            .onAllNodesWithText("100 % de réussite")
                            .fetchSemanticsNodes()
                            .isNotEmpty()
                    }
                    compose.onNodeWithText("Lancer maintenant").assertIsDisplayed()
                    capture("mission-sheet-phone-dark")
                    compose.onNodeWithContentDescription("Fermer").performClick()
                    compose.waitUntil(15000) {
                        compose.onAllNodesWithTag("mission-sheet").fetchSemanticsNodes().isEmpty()
                    }
                    compose.onNodeWithContentDescription("Créer une mission").performClick()
                    compose
                        .onNode(hasText("Agent principal") and hasClickAction())
                        .assertIsDisplayed()
                    capture("mission-editor-phone-dark")
                }
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun SemanticsNodeInteraction.assertCompactHeight(maximum: androidx.compose.ui.unit.Dp) {
        val bounds = getUnclippedBoundsInRoot()
        val measured = bounds.bottom - bounds.top
        org.junit.Assert.assertTrue(
            "Expected height <= $maximum, measured $measured",
            measured <= maximum,
        )
    }

    private fun waitDescription(value: String, substring: Boolean = false) {
        compose.waitUntil(15000) {
            compose
                .onAllNodesWithContentDescription(value, substring = substring)
                .fetchSemanticsNodes()
                .isNotEmpty()
        }
    }

    private fun waitText(value: String) {
        compose.waitUntil(15000) {
            compose.onAllNodesWithText(value).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun awaitMarkdown(activity: ComponentActivity, text: String) {
        fun textViews(view: View): List<TextView> =
            when (view) {
                is TextView -> listOf(view)
                is ViewGroup -> (0 until view.childCount).flatMap { textViews(view.getChildAt(it)) }
                else -> emptyList()
            }
        compose.waitUntil(15000) {
            compose.onRoot().fetchSemanticsNode()
            textViews(activity.window.decorView).any { it.text.contains(text) }
        }
    }

    private fun capture(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        val target =
            if (compose.onAllNodes(isDialog()).fetchSemanticsNodes().isNotEmpty())
                compose.onNode(isDialog())
            else compose.onRoot()
        target
            .captureToImage()
            .asAndroidBitmap()
            .compress(
                android.graphics.Bitmap.CompressFormat.PNG,
                100,
                File(dir, "$name.png").outputStream(),
            )
    }
}
