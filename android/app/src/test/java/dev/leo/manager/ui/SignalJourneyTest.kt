package dev.leo.manager.ui

import android.app.Application
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
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

/** Journeys through the redesigned home, search, new conversation, missions and Atelier. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class SignalJourneyTest {
    @get:Rule val compose = createComposeRule()

    private val calls = CopyOnWriteArrayList<Triple<String, String, String>>()
    private val now = System.currentTimeMillis()
    private val main = Agent(MAIN_AGENT_ID, "Agent principal")
    private val designer =
        Agent("designer", "Designer", provider = "claude", access = AccessPolicy(projects = listOf("site")))
    private val manager = Project("manager", "Leo Agent Manager")
    private val site = Project("site", "Site vitrine")
    private val daily =
        Task("daily", "Revue quotidienne", "Relire les changements du jour.", MAIN_AGENT_ID, cron = "0 9 * * *", nextRun = now + 3_600_000)
    private val failed =
        Run("failed-run", taskId = "daily", status = "failed", trigger = "schedule", startedAt = now - 7_200_000,
            finishedAt = now - 7_000_000, snapshot = Snapshot(task = daily, agent = main))
    private val chats =
        listOf(
            Chat("question", "Choisir la pagination", agentId = "designer", agentName = "Designer", pendingQuestions = 1, updatedAt = now - 60_000),
            Chat("live", "Refonte de l’application", agentName = "Agent principal", projectName = "Leo Agent Manager",
                status = "running", runId = "live-run", run = Run("live-run", status = "running", startedAt = now - 14 * 60_000), updatedAt = now - 120_000),
            Chat("done", "Relecture de la PR", agentName = "Agent principal", updatedAt = now - 90_000_000),
        )

    @org.junit.Before
    fun prepare() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        runBlocking { Preferences(app).setOrigin("") }
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            app,
            androidx.work.Configuration.Builder().setExecutor(androidx.work.testing.SynchronousExecutor()).build(),
        )
    }

    @org.junit.After
    fun finish() = androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()

    private fun stream(state: LiveState): MockResponse {
        val frame = "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), state, true, false))}\n\n"
        return MockResponse()
            .setHeader("Content-Type", "text/event-stream")
            .setBody(frame + ": keepalive\n\n".repeat(100000))
            .throttleBody(frame.toByteArray().size.toLong(), 1, TimeUnit.SECONDS)
    }

    private fun json(body: String) = MockResponse().setHeader("Content-Type", "application/json").setBody(body)

    private fun journey(claudeConnected: Boolean = true, body: (LeoViewModel) -> Unit) {
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        calls += Triple(request.method!!, request.path!!, request.body.readUtf8())
                        return when {
                            path == "/api/session" -> json("{\"authenticated\":true,\"csrf\":\"fixture\"}")
                            path == "/api/agents" -> json(wireJson.encodeToString(listOf(main, designer)))
                            path == "/api/projects" -> json(wireJson.encodeToString(listOf(manager, site)))
                            path == "/api/tasks" && request.method == "GET" -> json(wireJson.encodeToString(listOf(daily)))
                            path == "/api/tasks/daily" && request.method == "PUT" -> json(wireJson.encodeToString(daily.copy(enabled = false)))
                            path == "/api/tasks/activity" -> json(wireJson.encodeToString(listOf(failed)))
                            path == "/api/tasks/daily/run" ->
                                json(wireJson.encodeToString(Run("new-run", taskId = "daily", status = "running", snapshot = Snapshot(task = daily, agent = main))))
                            path == "/api/runs" -> json(wireJson.encodeToString(listOf(failed, failed.copy(id = "older", status = "succeeded"))))
                            path == "/api/schedule/preview" -> json("{\"occurrences\":[${now + 3_600_000},${now + 90_000_000}]}")
                            path == "/api/skills" -> json(wireJson.encodeToString(listOf(Skill("revue", "Guide de revue"))))
                            path == "/api/mcps" -> json("[]")
                            path == "/api/chats/stream" -> stream(LiveState(chats = chats))
                            path == "/api/chats" && request.method == "POST" -> json(wireJson.encodeToString(Chat("created", agentId = "designer")))
                            path.startsWith("/api/chats/") && path.endsWith("/stream") -> {
                                val id = path.removePrefix("/api/chats/").removeSuffix("/stream")
                                stream(LiveState(chat = chats.find { it.id == id } ?: Chat(id, agentId = "designer")))
                            }
                            path.startsWith("/api/chats/") && path.endsWith("/messages") -> json("{}")
                            path.startsWith("/api/runs/") && path.endsWith("/stream") ->
                                stream(LiveState(run = Run(path.removePrefix("/api/runs/").removeSuffix("/stream"), status = "running", snapshot = Snapshot(task = daily, agent = main))))
                            path == "/api/accounts" ->
                                json(
                                    "{\"accounts\":[{\"id\":\"a\",\"provider\":\"codex\",\"name\":\"Pro\",\"state\":\"ready\",\"remainingPercent\":64.0,\"stale\":false}" +
                                        (if (claudeConnected) ",{\"id\":\"c\",\"provider\":\"claude\",\"name\":\"Claude\",\"state\":\"ready\"}" else "") +
                                        "],\"required\":[" + (if (claudeConnected) "" else "\"claude\"") + "]}"
                                )
                            path == "/api/onepassword" -> json("[]")
                            else -> json("{}")
                        }
                    }
                }
            val vm = LeoViewModel(ApplicationProvider.getApplicationContext<Application>(), MemoryVault())
            compose.setContent {
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                LeoTheme("light") { LeoApp(vm = vm) }
            }
            try {
                body(vm)
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun waitText(value: String, substring: Boolean = false) =
        compose.waitUntil(15000) { compose.onAllNodesWithText(value, substring = substring).fetchSemanticsNodes().isNotEmpty() }

    private fun waitDescription(value: String, substring: Boolean = false) =
        compose.waitUntil(15000) {
            compose.onAllNodesWithContentDescription(value, substring = substring).fetchSemanticsNodes().isNotEmpty()
        }

    private fun waitCall(method: String, path: String) =
        compose.waitUntil(15000) {
            // Reading the tree lets Robolectric run the main looper, where request coroutines resume.
            // Sheets add a second root, so read them all.
            compose.onAllNodes(isRoot()).fetchSemanticsNodes()
            calls.any { it.first == method && it.second.startsWith(path) }
        }

    private fun capture(name: String) {
        val dir = System.getProperty("leo.screenshots.dir") ?: return
        compose.waitForIdle()
        File(dir).mkdirs()
        compose.onRoot().captureToImage().asAndroidBitmap()
            .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, File(dir, "$name.png").outputStream())
    }

    @Test
    fun `fil groups what needs the user, live work and recent conversations`() = journey {
        waitText("Choisir la pagination")
        compose.onNodeWithText("POUR VOUS").assertExists()
        compose.onNodeWithText("EN COURS").assertExists()
        compose.onNodeWithText("RÉCENTS").assertExists()
        // Conversations and mission activity arrive through separate requests.
        waitText("1 agent au travail · 2 éléments pour vous")
        compose.onNodeWithText("Une question vous attend").assertExists()
        compose.onNodeWithText("14 min").assertExists()
        compose.onNodeWithText("Revue quotidienne").assertExists()
        // The running conversation animates in the Fil: comet ring and "En cours" with its wave.
        compose.onAllNodesWithTag("chat-working-avatar", useUnmergedTree = true).assertCountEquals(1)
        assertTrue(compose.onAllNodesWithText("En cours", useUnmergedTree = true).fetchSemanticsNodes().isNotEmpty())
        capture("fil-light")
        // A failed mission is retried in place and its new run opens.
        compose.onNodeWithContentDescription("Relancer Revue quotidienne").performClick()
        waitCall("POST", "/api/tasks/daily/run")
        waitDescription("Retour")
        compose.onNodeWithContentDescription("Retour").performClick()
        // A running conversation shows the live working indicator.
        waitText("Refonte de l’application")
        compose.onNodeWithText("Refonte de l’application").performClick()
        compose.waitUntil(15000) { compose.onAllNodesWithTag("agent-working").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("agent-working").assert(hasContentDescription("Agent principal travaille"))
        compose.onNodeWithContentDescription("Retour").performClick()
        // Answering a question opens its conversation.
        waitText("Répondre")
        compose.onNodeWithText("Répondre").performClick()
        waitDescription("Changer de conversation : Choisir la pagination")
    }

    @Test
    fun `search finds missions and conversations and launches a mission`() = journey {
        waitText("Choisir la pagination")
        compose.onNodeWithContentDescription("Rechercher").performClick()
        compose.onNodeWithTag("search-field").performTextInput("revue")
        waitText("Lancer « Revue quotidienne »")
        compose.onNodeWithText("Guide de revue").assertExists()
        capture("search-light")
        compose.onNodeWithTag("search-field").performTextReplacement("pagination")
        waitText("CONVERSATIONS")
        compose.onNodeWithText("Lancer « Revue quotidienne »").assertDoesNotExist()
        compose.onNodeWithTag("search-field").performTextReplacement("revue")
        compose.onNodeWithText("Lancer « Revue quotidienne »").performClick()
        waitCall("POST", "/api/tasks/daily/run")
    }

    @Test
    fun `new conversation chooses the agent and project on screen`() = journey {
        waitText("Choisir la pagination")
        compose.onNodeWithContentDescription("Nouvelle conversation").performClick()
        waitText("On lance quoi ?")
        compose.onNodeWithTag("new-conversation").assertExists()
        // The main agent may use every project.
        compose.onNodeWithText("Leo Agent Manager").assertExists()
        compose.onNodeWithText("Designer").performClick()
        // The designer's access policy only allows the site project.
        compose.onNodeWithText("Leo Agent Manager").assertDoesNotExist()
        compose.onNodeWithText("Site vitrine").performClick()
        compose.onNode(hasSetTextAction()).performTextInput("Préparer la nouvelle page d’accueil")
        capture("new-conversation-light")
        compose.onNodeWithContentDescription("Envoyer").performClick()
        waitCall("POST", "/api/chats")
        val created = wireJson.parseToJsonElement(calls.first { it.first == "POST" && it.second == "/api/chats" }.third).jsonObject
        assertEquals("designer", created["agentId"]?.jsonPrimitive?.content)
        assertEquals("site", created["projectId"]?.jsonPrimitive?.content)
        waitCall("POST", "/api/chats/created/messages")
        val message = wireJson.parseToJsonElement(calls.first { it.second == "/api/chats/created/messages" }.third).jsonObject
        assertEquals("Préparer la nouvelle page d’accueil", message["text"]?.jsonPrimitive?.content)
        assertEquals("claude", message["provider"]?.jsonPrimitive?.content)
    }

    @Test
    fun `mission sheet shows the schedule and history and pauses the mission`() = journey { vm ->
        waitDescription("Missions")
        compose.onNodeWithContentDescription("Missions").performClick()
        // The strip appears once the recent runs are loaded.
        waitDescription("1 réussies sur 2 dernières exécutions")
        compose.onNodeWithText("Tous les jours · 09:00").assertExists()
        compose.onNodeWithText("Revue quotidienne").performClick()
        compose.onNodeWithTag("mission-sheet").assertIsDisplayed()
        waitText("50 % de réussite")
        compose.onNodeWithText("09:00").assertExists()
        // The failure is flagged on the card and listed in the history.
        compose.onAllNodesWithText("Échec").assertCountEquals(2)
        capture("mission-sheet-light")
        compose.onNodeWithContentDescription("Mettre en pause").performScrollTo().performClick()
        waitCall("PUT", "/api/tasks/daily")
        val saved = wireJson.parseToJsonElement(calls.first { it.first == "PUT" && it.second == "/api/tasks/daily" }.third).jsonObject
        assertEquals("false", saved["enabled"]?.jsonPrimitive?.content)
        // Actions stay disabled while the save refreshes the workspace.
        compose.waitUntil(15000) {
            compose.onAllNodes(hasText("Lancer maintenant") and isEnabled()).fetchSemanticsNodes().isNotEmpty() && !vm.state.value.busy
        }
        compose.onNodeWithText("Lancer maintenant").performScrollTo().performClick()
        waitCall("POST", "/api/tasks/daily/run")
        waitDescription("Retour")
    }

    @Test
    fun `atelier leads with connection health and opens each resource`() = journey(claudeConnected = false) {
        waitDescription("Atelier")
        compose.onNodeWithContentDescription("Atelier").performClick()
        waitText("Compte Claude Code requis")
        waitText("1/1 prêt · 64 % restant")
        // Claude Code and 1Password, which loads after the coding-agent accounts.
        compose.waitUntil(15000) { compose.onAllNodesWithText("Aucun compte", useUnmergedTree = true).fetchSemanticsNodes().size == 2 }
        capture("atelier-light")
        compose.onNodeWithText("Journal des exécutions").performScrollTo().performClick()
        waitText("Journal")
        waitCall("GET", "/api/runs?limit=30")
    }
}
