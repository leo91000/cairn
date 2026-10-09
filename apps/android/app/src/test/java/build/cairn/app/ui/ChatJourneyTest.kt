package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ChatJourneyTest {
    @get:Rule val compose = createComposeRule()

    @org.junit.Before
    fun initializeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @org.junit.After
    fun closeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun `native chat sends a message answers questions edits queue and opens a versioned artifact`() {
        MockWebServer().use { server ->
            val chatId = "00000000-0000-4000-8000-000000000088"
            val calls = CopyOnWriteArrayList<Triple<String, String, String>>()
            var answered = false
            var edited = false
            val question =
                ChatQuestion(
                    "question",
                    chatId,
                    blocking = true,
                    fields =
                        listOf(
                            QuestionField(
                                "scope",
                                "Quelle portée pour la revue ?",
                                options =
                                    listOf(
                                        QuestionOption(
                                            "Application entière",
                                            "Inclure le client Android et le serveur",
                                        )
                                    ),
                            )
                        ),
                )
            val artifact =
                Deliverable(
                    "report-v2",
                    "run",
                    key = "report",
                    version = 2,
                    title = "Compte rendu",
                    name = "rapport.md",
                    group = "Revue",
                    kind = "markdown",
                    mediaType = "text/markdown",
                    size = 140,
                    createdAt = 1789315200000,
                    excerpt = "La revue du projet est terminée.",
                )
            val picture =
                Deliverable(
                    "image",
                    "run",
                    key = "image",
                    version = 1,
                    title = "Aperçu du projet",
                    name = "projet.png",
                    group = "Revue",
                    kind = "image",
                    mediaType = "image/png",
                    previewStatus = "ready",
                    createdAt = 1789315200001,
                )
            val pictureBytes =
                java.io
                    .ByteArrayOutputStream()
                    .also { output ->
                        val bitmap =
                            android.graphics.Bitmap.createBitmap(
                                600,
                                360,
                                android.graphics.Bitmap.Config.ARGB_8888,
                            )
                        val canvas = android.graphics.Canvas(bitmap)
                        canvas.drawColor(android.graphics.Color.rgb(69, 69, 239))
                        val paint =
                            android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG).apply {
                                color = android.graphics.Color.WHITE
                                textSize = 48f
                            }
                        canvas.drawText("Cairn · Android", 48f, 110f, paint)
                        paint.alpha = 100
                        canvas.drawRoundRect(48f, 165f, 552f, 210f, 16f, 16f, paint)
                        canvas.drawRoundRect(48f, 235f, 380f, 280f, 16f, 16f, paint)
                        bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, output)
                        bitmap.recycle()
                    }
                    .toByteArray()
            val event =
                RunEvent(
                    1,
                    1789315200000,
                    "item.completed",
                    "",
                    mapOf(
                        "item" to
                            buildJsonObject {
                                put("id", "answer")
                                put("type", "agent_message")
                                put(
                                    "text",
                                    "J’ai terminé la **revue du projet**. Les changements sont cohérents et les vérifications passent.\n\nLes points essentiels :\n- Une conversation plus lisible\n- Les fichiers regroupés au même endroit\n\n[Ouvrir le compte rendu](/api/installations/fixture/api/runs/run/artifacts/report-v2).",
                                )
                            }
                    ),
                )
            fun stream(state: LiveState, events: List<RunEvent> = emptyList()): MockResponse {
                val body =
                    "event: batch\nid: ${events.lastOrNull()?.id ?: 0}\ndata: ${wireJson.encodeToString(LiveBatch(events,state,false,false))}\n\n"
                return MockResponse()
                    .setHeader("Content-Type", "text/event-stream")
                    .setBody(body + ": keepalive\n\n")
                    .throttleBody(
                        body.toByteArray().size.toLong(),
                        1,
                        java.util.concurrent.TimeUnit.SECONDS,
                    )
            }
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        val method = request.method.orEmpty()
                        if (method in listOf("POST", "PUT", "DELETE")) {
                            if (
                                request.getHeader("Cookie") != "cairn_session=fixture" ||
                                    request.getHeader("X-CSRF-Token") != "csrf-fixture"
                            )
                                return MockResponse().setResponseCode(403).setBody("{}")
                            calls += Triple(method, path, request.body.readUtf8())
                        }
                        val chat =
                            Chat(
                                chatId,
                                title = "Revue du projet",
                                agentName = "Cairn",
                                status = "running",
                                runId = "run",
                                run = Run("run", status = "running", trigger = "chat"),
                                questions = if (answered) emptyList() else listOf(question),
                                messages =
                                    if (edited) emptyList()
                                    else
                                        listOf(
                                            ChatMessage(
                                                "queued",
                                                chatId,
                                                text = "Vérifier les tests",
                                                status = "queued",
                                            )
                                        ),
                                pendingQuestions = if (answered) 0 else 1,
                            )
                        val value =
                            when (path) {
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("csrf-fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(id = MAIN_AGENT_ID, name = "Cairn"))
                                    )
                                "/api/installations/fixture/api/projects",
                                "/api/installations/fixture/api/tasks",
                                "/api/installations/fixture/api/tasks/activity",
                                "/api/installations/fixture/api/skills",
                                "/api/installations/fixture/api/mcps" -> "[]"
                                "/api/installations/fixture/api/overview" -> "{}"
                                "/api/installations/fixture/api/codex/models" ->
                                    wireJson.encodeToString(
                                        ModelCatalog(
                                            models =
                                                listOf(
                                                    CodexModel(
                                                        "gpt-fixture",
                                                        displayName = "Modèle du serveur",
                                                        isDefault = true,
                                                        supportedReasoningEfforts =
                                                            listOf(
                                                                ReasoningOption("low"),
                                                                ReasoningOption("high"),
                                                                ReasoningOption("max"),
                                                            ),
                                                    )
                                                )
                                        )
                                    )
                                "/api/installations/fixture/api/chats/stream" ->
                                    return stream(LiveState(chats = listOf(chat)))
                                "/api/installations/fixture/api/chats" ->
                                    wireJson.encodeToString(chat)
                                "/api/installations/fixture/api/chats/$chatId/stream" ->
                                    return stream(
                                        LiveState(
                                            chat = chat,
                                            run = chat.run,
                                            artifacts =
                                                listOf(
                                                    artifact.copy(id = "report-v1", version = 1),
                                                    artifact,
                                                    picture,
                                                ),
                                        ),
                                        listOf(
                                            RunEvent(
                                                9,
                                                1789315189000,
                                                "chat.user",
                                                "Peux-tu examiner le projet et préparer un compte rendu ?",
                                            ),
                                            RunEvent(10, 1789315190000, "turn.started", ""),
                                            RunEvent(
                                                11,
                                                1789315190100,
                                                "item.started",
                                                "",
                                                mapOf(
                                                    "item" to
                                                        buildJsonObject {
                                                            put("id", "command-1")
                                                            put("type", "command_execution")
                                                            put("command", "git status --short")
                                                            put("status", "in_progress")
                                                        }
                                                ),
                                            ),
                                            RunEvent(
                                                12,
                                                1789315190200,
                                                "item.completed",
                                                "",
                                                mapOf(
                                                    "item" to
                                                        buildJsonObject {
                                                            put("id", "command-1")
                                                            put("type", "command_execution")
                                                            put("command", "git status --short")
                                                            put("status", "completed")
                                                            put(
                                                                "aggregated_output",
                                                                "Workspace clean",
                                                            )
                                                        }
                                                ),
                                            ),
                                            event.copy(id = 13),
                                        ),
                                    )
                                "/api/installations/fixture/api/chats/$chatId/messages" -> "{}"
                                "/api/installations/fixture/api/chats/$chatId/questions/question/answer" -> {
                                    answered = true
                                    "{}"
                                }
                                "/api/installations/fixture/api/chats/$chatId/messages/queued" -> {
                                    edited = true
                                    "{}"
                                }
                                "/api/installations/fixture/api/runs/run/artifacts/image" ->
                                    return MockResponse()
                                        .setHeader("Content-Type", "image/png")
                                        .setBody(okio.Buffer().write(pictureBytes))
                                "/api/installations/fixture/api/runs/run/artifacts/report-v2" ->
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/markdown")
                                        .setBody(
                                            "# Compte rendu\n\nLes conversations et les artifacts sont disponibles dans Android.\n\n- Interface native\n- Pièces jointes\n- Notifications périodiques\n"
                                        )
                                else ->
                                    return MockResponse()
                                        .setResponseCode(404)
                                        .setBody("{\"error\":\"Unknown fixture\"}")
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(value)
                    }
                }
            server.start()
            val vault =
                MemoryVault().apply {
                    write(server.url("/").toString(), "cairn_session=fixture; Path=/; Max-Age=3600")
                }
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    vault,
                    beaconOrigin = "",
                )
            var compact by mutableStateOf(false)
            compose.setContent {
                val theme by vm.theme.collectAsStateWithLifecycle("system")
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme(theme) {
                    Box(
                        Modifier.fillMaxWidth()
                            .height(if (compact) 520.dp else 915.dp)
                            .testTag("app-viewport")
                    ) {
                        CairnApp(vm = vm)
                    }
                }
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithContentDescription("Nouvelle conversation")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Nouvelle conversation").performClick()
            compose.waitUntil(10_000) {
                compose.onAllNodes(hasSetTextAction()).fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNode(hasSetTextAction())
                .performTextInput("Examiner le projet et préparer un compte rendu")
            compose.onNodeWithTag("model-picker").performClick()
            compose.onNode(hasText("Modèle du serveur") and isSelectable()).performClick()
            compose.onNodeWithTag("reasoning-slider").performSemanticsAction(
                androidx.compose.ui.semantics.SemanticsActions.SetProgress
            ) {
                it(2f)
            }
            compose.onNodeWithText("Terminé").performClick()
            compose.onNodeWithContentDescription("Envoyer").performClick()
            waitText("1 question · Répondre")
            assertTrue(
                calls.any {
                    it.first == "POST" && it.second == "/api/installations/fixture/api/chats"
                }
            )
            val sent =
                wireJson
                    .parseToJsonElement(calls.first { it.second.endsWith("/messages") }.third)
                    .jsonObject
            assertEquals("gpt-fixture", sent["model"]?.jsonPrimitive?.content)
            assertEquals("max", sent["reasoning"]?.jsonPrimitive?.content)
            assertEquals(
                "Examiner le projet et préparer un compte rendu",
                sent["text"]?.jsonPrimitive?.content,
            )
            compose.onNodeWithText("Chats").assertDoesNotExist()
            compose.onNodeWithContentDescription("Actualiser l’espace").assertDoesNotExist()
            screenshot("chat")
            compose
                .onNodeWithTag("conversation-history")
                .performScrollToNode(hasTestTag("agent-actions"))
            compose.runOnIdle {
                val activity =
                    androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry.getInstance()
                        .getActivitiesInStage(androidx.test.runner.lifecycle.Stage.RESUMED)
                        .first()
                fun descendants(view: android.view.View): Sequence<android.view.View> = sequence {
                    yield(view)
                    if (view is android.view.ViewGroup)
                        for (index in 0 until view.childCount) yieldAll(
                            descendants(view.getChildAt(index))
                        )
                }
                val view =
                    descendants(activity.window.decorView)
                        .filterIsInstance<android.widget.TextView>()
                        .first { it.text.contains("Ouvrir le compte rendu") }
                val text = view.text as android.text.Spanned
                text
                    .getSpans(0, text.length, android.text.style.ClickableSpan::class.java)
                    .first()
                    .onClick(view)
            }
            waitText("rapport.md")
            compose.onNodeWithContentDescription("Fermer le fichier").performClick()
            compose.onNodeWithText("1 question · Répondre").performClick()
            compose.onNodeWithText("Application entière").performClick()
            compose.onNodeWithText("Envoyer la réponse").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Votre réponse").fetchSemanticsNodes().isEmpty()
            }
            val answer =
                wireJson
                    .parseToJsonElement(calls.first { it.second.endsWith("/answer") }.third)
                    .jsonObject
            assertEquals(
                "Application entière",
                answer["answers"]!!.jsonObject["scope"]!!.jsonArray.single().jsonPrimitive.content,
            )
            // Queued follow-ups stay next to the composer, outside the scrolling history.
            compose.onNodeWithTag("conversation-queue").assertIsDisplayed()
            compose.onNodeWithTag("queued-message").performClick()
            compose.onNodeWithText("Modifier", substring = false).performClick()
            compose
                .onNode(hasText("Vérifier les tests") and hasSetTextAction())
                .performTextReplacement("Vérifier aussi les fichiers")
            compose.onNodeWithContentDescription("Modifier").performClick()
            compose.waitUntil(10000) {
                calls.any { it.first == "PUT" && it.second.endsWith("/messages/queued") }
            }
            compose.onNodeWithContentDescription("Fichiers · 3").performClick()
            waitText("Compte rendu")
            compose.onAllNodesWithText("Compte rendu").onLast().assertExists()
            screenshot("artifacts", true)
            compose.onAllNodesWithText("Compte rendu").onLast().performClick()
            waitText("rapport.md")
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasContentDescription("Enregistrer") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            screenshot("artifact-preview", true)
            compose.onNodeWithContentDescription("Suivant").performClick()
            waitText("projet.png")
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithContentDescription("projet.png")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            screenshot("artifact-image", true)
            compose.onNodeWithContentDescription("Fermer le fichier").performClick()
            vm.setTheme("dark")
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithContentDescription("Fermer les artifacts")
                    .fetchSemanticsNodes()
                    .isNotEmpty() && kotlinx.coroutines.runBlocking { vm.theme.first() == "dark" }
            }
            compose.waitForIdle()
            compose.onNodeWithContentDescription("Fermer les artifacts").performClick()
            screenshot("chat-dark")
            compose
                .onNodeWithTag("conversation-history")
                .performScrollToNode(hasTestTag("agent-actions"))
            // Collapsed, the actions read as one sentence; expanded, as a timeline of steps.
            compose
                .onNodeWithTag("agent-actions")
                .assert(hasText("A lancé 1 commande"))
                .performClick()
            compose.onNodeWithText("État du dépôt Git").assertExists()
            compose.onNodeWithText("État du dépôt Git").performClick()
            compose.onNodeWithTag("agent-step-sheet").assertExists()
            compose.onNodeWithText("Workspace clean").assertExists()
            screenshot("chat-tools-dark")
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("agent-step-sheet").fetchSemanticsNodes().isEmpty()
            }
            compose.onNodeWithTag("agent-actions").performClick()
            compose
                .onNode(hasSetTextAction())
                .performTextInput(
                    "Peux-tu vérifier les changements, regrouper les résultats et préparer une version Android plus lisible ?\nLe chat doit garder de la place pendant la saisie.\nEt préserver les actions utiles.\nMerci !\nUne dernière ligne."
                )
            screenshot("chat-draft-dark")
            compose.runOnIdle { compact = true }
            compose.waitForIdle()
            compose.onNodeWithContentDescription("Ajouter à la file").assertIsDisplayed()
            compose.onNodeWithContentDescription("Options de la conversation").assertIsDisplayed()
            System.getProperty("cairn.screenshots.dir")?.let { dir ->
                compose
                    .onNodeWithTag("app-viewport")
                    .captureToImage()
                    .asAndroidBitmap()
                    .compress(
                        android.graphics.Bitmap.CompressFormat.PNG,
                        100,
                        File(dir, "chat-compact-dark.png").outputStream(),
                    )
            }
            compose.runOnIdle { compact = false }
            compose.onNodeWithContentDescription("Intervenir maintenant").performClick()
            compose.waitUntil(10000) {
                calls.any {
                    it.second.endsWith("/messages") &&
                        wireJson
                            .parseToJsonElement(it.third)
                            .jsonObject["mode"]
                            ?.jsonPrimitive
                            ?.content == "steer"
                }
            }
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasSetTextAction() and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty() && !vm.state.value.busy
            }
            compose.onNode(hasSetTextAction()).performTextInput("Brouillon à conserver")
            compose.onNode(hasSetTextAction() and hasText("Brouillon à conserver")).assertExists()
            compose.onNodeWithContentDescription("Options de la conversation").performClick()
            compose.onNodeWithText("Nouvelle conversation").performClick()
            assertEquals("Brouillon à conserver", vm.chatDrafts[chatId]?.text)
            compose
                .onNode(hasContentDescription("Changer de conversation", substring = true))
                .performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Revue du projet").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onAllNodesWithText("Revue du projet").onLast().performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasSetTextAction() and hasText("Brouillon à conserver"))
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNode(hasSetTextAction() and hasText("Brouillon à conserver")).assertExists()
            vm.setTheme("system")
            vm.api.closeStreams()
        }
    }

    private fun waitText(text: String) {
        compose.waitUntil(10000) {
            compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun screenshot(name: String, dialog: Boolean = false) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        if (!dialog)
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Reconnexion…").fetchSemanticsNodes().isEmpty()
            }
        File(dir).mkdirs()
        (if (dialog) compose.onNode(isDialog()) else compose.onRoot())
            .captureToImage()
            .asAndroidBitmap()
            .let { bitmap ->
                File(dir, "$name.png").outputStream().use {
                    bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
                }
            }
    }
}
