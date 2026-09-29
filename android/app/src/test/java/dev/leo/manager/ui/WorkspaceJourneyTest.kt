package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.getValue
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
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
class WorkspaceJourneyTest {
    @get:Rule val compose = createComposeRule()

    @org.junit.Before
    fun initializeWork() {
        runBlocking {
            Preferences(ApplicationProvider.getApplicationContext<Application>()).setTheme("system")
        }
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
    fun `owner signs in creates a task and follows its execution through native screens`() {
        MockWebServer().use { server ->
            val mutations = CopyOnWriteArrayList<Pair<String, String>>()
            var created = false
            var launched = false
            val existingTask =
                """{"id":"older-task","name":"Mission précédente","prompt":"Ancienne mission","agentId":"agent"}"""
            val task =
                """{"id":"task","name":"Nouvelle mission","prompt":"Vérifier le projet","agentId":"agent","projectId":null,"skills":null}"""
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path.orEmpty().substringBefore('?')
                        if (request.method == "POST" && path != "/api/login") {
                            if (
                                request.getHeader("Cookie") != "leo_session=test-cookie" ||
                                    request.getHeader("X-CSRF-Token") != "test-csrf"
                            ) {
                                return MockResponse()
                                    .setResponseCode(403)
                                    .setBody("""{"error":"Missing session or CSRF"}""")
                            }
                            mutations += path to request.body.readUtf8()
                        }
                        if (request.method == "POST" && path == "/api/tasks/task/run")
                            launched = true
                        val data =
                            when (path) {
                                "/api/session" ->
                                    """{"authenticated":false,"setupRequired":false}"""
                                "/api/login" ->
                                    return MockResponse()
                                        .addHeader(
                                            "Set-Cookie",
                                            "leo_session=test-cookie; Path=/; Max-Age=3600; HttpOnly",
                                        )
                                        .setBody("""{"authenticated":true,"csrf":"test-csrf"}""")
                                "/api/agents" -> """[{"id":"agent","name":"Reviewer"}]"""
                                "/api/projects" ->
                                    """[{"id":"project","name":"Leo Agent Manager","path":"/fixtures/leo"}]"""
                                "/api/mcps",
                                "/api/skills",
                                "/api/tokens",
                                "/api/audit" -> "[]"
                                "/api/tasks/activity" ->
                                    if (launched)
                                        "[{\"id\":\"run\",\"taskId\":\"task\",\"status\":\"running\"}]"
                                    else "[]"
                                "/api/runs" ->
                                    if (launched)
                                        "[{\"id\":\"run\",\"taskId\":\"task\",\"status\":\"running\"}]"
                                    else "[]"
                                "/api/codex/models" -> "{\"models\":[]}"
                                "/api/chats/stream" ->
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/event-stream")
                                        .setBody(
                                            "event: batch\nid: 0\ndata: {\"events\":[],\"state\":{\"chats\":[],\"artifacts\":[]},\"reset\":false,\"more\":false}\n\n"
                                        )
                                "/api/runs/run/stream" ->
                                    return MockResponse()
                                        .setHeader("Content-Type", "text/event-stream")
                                        .setBody(
                                            "event: batch\nid: 3\ndata: " +
                                                wireJson.encodeToString(
                                                    LiveBatch(
                                                        listOf(
                                                            RunEvent(
                                                                1,
                                                                1789315200000,
                                                                "run.started",
                                                                "Le worker démarre la mission",
                                                            ),
                                                            RunEvent(
                                                                2,
                                                                1789315201000,
                                                                "item.started",
                                                                "",
                                                                mapOf(
                                                                    "item" to
                                                                        buildJsonObject {
                                                                            put("id", "check")
                                                                            put(
                                                                                "type",
                                                                                "command_execution",
                                                                            )
                                                                            put(
                                                                                "command",
                                                                                "./gradlew testDebugUnitTest",
                                                                            )
                                                                            put(
                                                                                "status",
                                                                                "in_progress",
                                                                            )
                                                                        }
                                                                ),
                                                            ),
                                                            RunEvent(
                                                                3,
                                                                1789315202000,
                                                                "item.completed",
                                                                "",
                                                                mapOf(
                                                                    "item" to
                                                                        buildJsonObject {
                                                                            put("id", "progress")
                                                                            put(
                                                                                "type",
                                                                                "agent_message",
                                                                            )
                                                                            put(
                                                                                "text",
                                                                                "J’ai parcouru les changements. La structure du projet est cohérente.\n\nJe vérifie maintenant **les parcours principaux** :\n- Connexion et navigation\n- Envoi des messages\n- Lecture et partage des fichiers\n\nLe compte rendu suivra dès la fin des vérifications.",
                                                                            )
                                                                        }
                                                                ),
                                                            ),
                                                        ),
                                                        LiveState(
                                                            run =
                                                                Run(
                                                                    "run",
                                                                    status = "running",
                                                                    snapshot =
                                                                        Snapshot(
                                                                            task =
                                                                                wireJson
                                                                                    .decodeFromString(
                                                                                        task
                                                                                    ),
                                                                            agent =
                                                                                Agent(
                                                                                    name =
                                                                                        "Reviewer"
                                                                                ),
                                                                        ),
                                                                )
                                                        ),
                                                        false,
                                                        false,
                                                    )
                                                ) +
                                                "\n\n"
                                        )
                                "/api/settings" ->
                                    """{"publicUrl":"https://leo.example.com","mcpUrl":"https://leo.example.com/mcp","protocol":"2026-07-28","version":"0.19.0"}"""
                                "/api/overview" ->
                                    """{"counts":{},"agents":1,"projects":1,"tasks":[],"runs":[]}"""
                                "/api/tasks" ->
                                    if (request.method == "POST") {
                                        created = true
                                        task
                                    } else if (created) "[$existingTask,$task]"
                                    else "[$existingTask]"
                                "/api/tasks/task/run",
                                "/api/runs/run" ->
                                    """{"id":"run","taskId":"task","status":"running","snapshot":{"task":$task,"agent":{"name":"Reviewer"},"project":null,"projects":[{"name":"Leo Agent Manager"}]}}"""
                                "/api/runs/run/events" ->
                                    """[{"id":1,"createdAt":1789315200000,"type":"run.started","text":"Le worker démarre la mission"}]"""
                                else ->
                                    return MockResponse()
                                        .setResponseCode(404)
                                        .setBody("""{"error":"Unknown fixture endpoint"}""")
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(data)
                    }
                }
            server.start()
            val vm =
                LeoViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    MemoryVault(),
                )
            compose.setContent {
                val theme by vm.theme.collectAsStateWithLifecycle(initialValue = "system")
                LeoTheme(theme) { LeoApp(vm = vm) }
            }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Adresse du serveur").fetchSemanticsNodes().isNotEmpty()
            }
            compose
                .onNodeWithText("Adresse du serveur")
                .performTextInput(server.url("/").toString())
            compose.onNodeWithText("Continuer").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Mot de passe").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Mot de passe").performTextInput("test-only-password")
            compose.onNodeWithText("Se connecter").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithContentDescription("Missions")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            screenshot("overview")
            compose.onNodeWithContentDescription("Missions").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasContentDescription("Créer une mission") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Créer une mission").performClick()
            compose.onNodeWithText("Nom").performTextInput("Nouvelle mission")
            compose
                .onNodeWithText("Mission et critères de réussite")
                .performTextInput("Vérifier le projet")
            compose.onNodeWithText("Enregistrer").performClick()
            // The saved mission opens in its sheet, ready to run.
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Lancer maintenant")
                    .fetchSemanticsNodes()
                    .isNotEmpty() &&
                    !vm.state.value.busy &&
                    vm.state.value.tasks.any { it.id == "task" }
            }
            val saved =
                wireJson
                    .parseToJsonElement(mutations.first { it.first == "/api/tasks" }.second)
                    .jsonObject
            assertEquals("Nouvelle mission", saved["name"]?.jsonPrimitive?.content)
            assertTrue(
                saved["projectId"] == null ||
                    saved["projectId"] == kotlinx.serialization.json.JsonNull
            )
            assertTrue(
                saved["skills"] == null || saved["skills"] == kotlinx.serialization.json.JsonNull
            )
            compose.onNodeWithText("Lancer maintenant").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("agent-working").fetchSemanticsNodes().isNotEmpty()
            }
            // The indicator names the command still running in the fixture.
            compose
                .onNodeWithTag("agent-working")
                .assert(hasContentDescription("Exécuter les tests : ./gradlew testDebugUnitTest"))
            // The running command is carried by the indicator; the actions keep the finished steps.
            compose
                .onNodeWithTag("agent-actions")
                .assert(hasText("Suivi de l’exécution"))
                .performClick()
            compose.onNodeWithText("Exécuter les tests").assertDoesNotExist()
            compose.onNodeWithText("Travail commencé").performClick()
            compose.onNodeWithTag("agent-step-sheet").assertExists()
            compose.onNodeWithText("Le worker démarre la mission").assertExists()
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("agent-step-sheet").fetchSemanticsNodes().isEmpty()
            }
            compose.onNodeWithTag("agent-actions").performClick()
            assertTrue(mutations.any { it.first == "/api/tasks/task/run" })
            screenshot("run")
            compose.onNodeWithContentDescription("Retour").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithContentDescription("Atelier")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Atelier").performClick()
            compose.onNodeWithText("Paramètres et accès").performScrollTo().performClick()
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Système").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Système").performClick()
            compose.onNodeWithText("Sombre").performClick()
            val preferences = Preferences(ApplicationProvider.getApplicationContext<Application>())
            // Query semantics while waiting so Robolectric also drains the Android main looper.
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Sombre").fetchSemanticsNodes().size == 1 &&
                    runBlocking { preferences.theme.first() == "dark" }
            }
            compose.waitForIdle()
            screenshot("settings-dark")
            compose.onNodeWithContentDescription("Retour").performClick()
            compose.onNodeWithContentDescription("Fil").performClick()
            compose.waitForIdle()
            screenshot("overview-dark")
            runBlocking { preferences.setTheme("system") }
        }
    }

    private fun screenshot(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
        File(dir).mkdirs()
        compose.onRoot().captureToImage().asAndroidBitmap().let { bitmap ->
            File(dir, "$name.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }
}
