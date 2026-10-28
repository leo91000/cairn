package build.cairn.app.ui

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.File
import java.io.RandomAccessFile
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/**
 * Queued follow-ups while the agent works: readable text, one inline action, the rest in a sheet.
 */
abstract class QueueStripCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) = Unit

    private val long =
        "Également je ne vois pas de bouton pour mettre la file en pause sur Android, alors qu’il existe sur le web."
    private val messages =
        listOf(
            ChatMessage(id = "first", chatId = "chat", text = long),
            ChatMessage(
                id = "second",
                chatId = "chat",
                text = "Et relance la CI une fois que c’est fait.",
            ),
            ChatMessage(id = "third", chatId = "chat", text = "Puis publie les screenshots."),
        )

    @Test
    fun queuedMessagesStayReadableAndOfferEveryActionFromTheSheet() {
        MockWebServer().use { server ->
            val calls = CopyOnWriteArrayList<Triple<String, String, String>>()
            val chat =
                Chat(
                    id = "chat",
                    title = "Refonte de la file",
                    agentName = "Agent principal",
                    status = "running",
                    runId = "run",
                    run =
                        Run(
                            id = "run",
                            status = "running",
                            trigger = "chat",
                            taskId = "",
                        ),
                    messages = messages,
                    updatedAt = System.currentTimeMillis(),
                )
            // A neighbour enables the conversation pager, whose swipe must not steal the queue's.
            val neighbour =
                Chat(
                    id = "neighbour",
                    title = "Conversation voisine",
                    agentName = "Agent principal",
                    updatedAt = chat.updatedAt - 60000,
                )
            val events =
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = System.currentTimeMillis() - 60000,
                        type = "chat.user",
                        text = "Peux-tu revoir l’interface de la file d’attente ?",
                    ),
                    RunEvent(
                        id = 2,
                        createdAt = System.currentTimeMillis() - 50000,
                        type = "turn.started",
                        text = "",
                    ),
                    RunEvent(
                        id = 3,
                        createdAt = System.currentTimeMillis() - 40000,
                        type = "item.completed",
                        text = "",
                        payload =
                            kotlinx.serialization.json.JsonObject(
                                mapOf(
                                    "item" to
                                        buildJsonObject {
                                            put("id", "answer")
                                            put("type", "agent_message")
                                            put(
                                                "text",
                                                "Je regarde le composant actuel et je prépare une version plus lisible.",
                                            )
                                        }
                                )
                            ),
                    ),
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (request.method in listOf("POST", "PUT", "DELETE"))
                            calls += Triple(request.method!!, path, request.body.readUtf8())
                        fun stream(
                            state: LiveState,
                            events: List<RunEvent> = emptyList(),
                        ): MockResponse {
                            val frame =
                                "event: batch\nid: ${events.lastOrNull()?.id ?: 0}\ndata: ${wireJson.encodeToString(LiveBatch(events, state, true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame)
                        }
                        val body =
                            when (path) {
                                "/api/installations/fixture/api/chats/stream" ->
                                    return stream(LiveState(chats = listOf(chat, neighbour)))
                                "/api/installations/fixture/api/chats/neighbour/stream" ->
                                    return stream(LiveState(chat = neighbour))
                                "/api/installations/fixture/api/chats/chat/stream" ->
                                    return stream(LiveState(chat = chat, run = chat.run), events)
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(
                                            listOf(
                                                Agent(
                                                    exclusiveNodes = emptyList(),
                                                    plugins = emptyList(),
                                                    revision = "",
                                                    createdAt = 0,
                                                    access =
                                                        AccessPolicy(
                                                            mcpTools =
                                                                build.cairn.contracts.McpTools(
                                                                    emptyMap()
                                                                ),
                                                            projects = null,
                                                            skills = null,
                                                            mcps = null,
                                                        ),
                                                    id = MAIN_AGENT_ID,
                                                    name = "Agent principal",
                                                )
                                            )
                                        )
                                    )
                                "/api/installations/fixture/api/overview" -> "{}"
                                "/api/installations/fixture/api/chats/chat" ->
                                    wireJson.encodeToString(chat)
                                "/api/installations/fixture/api/projects" -> """{"projects":[]}"""
                                "/api/installations/fixture/api/tasks" -> """{"tasks":[]}"""
                                "/api/installations/fixture/api/mcps" -> """{"mcps":[]}"""
                                else ->
                                    if (path.endsWith("/models")) """{"models":[]}"""
                                    else if (request.method == "GET") "[]" else "{}"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    QueueVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                CairnTheme("dark") {
                    Surface(
                        Modifier.fillMaxSize().safeDrawingPadding(),
                        color = MaterialTheme.colorScheme.background,
                    ) {
                        if (state.session.authenticated)
                            ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
                    }
                }
            }
            // Reading semantics pumps the main looper, where requests resume and clear `busy`.
            fun settle() =
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithTag("conversation-queue")
                        .fetchSemanticsNodes()
                        .isNotEmpty() && !vm.state.value.busy
                }
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("conversation-queue").fetchSemanticsNodes().isNotEmpty()
            }
            compose.waitUntil(20000) {
                compose
                    .onAllNodesWithText("Peux-tu revoir", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText("Envoyé quand l’agent aura fini · 3").assertIsDisplayed()
            compose.onNodeWithText(long).assertIsDisplayed()
            compose.onNodeWithText("+ 2 autres messages").assertIsDisplayed()
            compose.onNodeWithText("Et relance la CI une fois que c’est fait.").assertDoesNotExist()
            // Only one inline action: editing and removal moved to the sheet.
            compose
                .onNodeWithContentDescription("Modifier le message en attente")
                .assertDoesNotExist()
            compose.onNodeWithContentDescription("Retirer le message").assertDoesNotExist()
            capture("queue-collapsed")

            compose.onNodeWithText("+ 2 autres messages").performClick()
            compose.onNodeWithText("Et relance la CI une fois que c’est fait.").assertIsDisplayed()
            compose.onNodeWithText("Puis publie les screenshots.").assertIsDisplayed()
            assertEquals(1, compose.onAllNodesWithText("Maintenant").fetchSemanticsNodes().size)
            capture("queue-expanded")
            compose.onNodeWithText("Réduire").performClick()
            compose.onNodeWithText("Puis publie les screenshots.").assertDoesNotExist()

            compose.onNodeWithText("Maintenant").performClick()
            compose.waitUntil(10000) {
                calls.any {
                    it.first == "PUT" &&
                        it.second == "/api/installations/fixture/api/chats/chat/messages/first"
                }
            }
            val steer =
                wireJson.parseToJsonElement(calls.first { it.first == "PUT" }.third).jsonObject
            assertEquals("steer", steer.getValue("mode").jsonPrimitive.content)
            assertEquals(long, steer.getValue("text").jsonPrimitive.content)
            settle()

            // A refused-only draft still belongs to the user and must block queue editing.
            val app = ApplicationProvider.getApplicationContext<Application>()
            val oversized = File(app.cacheDir, "cairn-files/queue-oversized.png")
            oversized.parentFile!!.mkdirs()
            RandomAccessFile(oversized, "rw").use { it.setLength(MAX_ATTACHMENT_BYTES + 1) }
            val uri = FileProvider.getUriForFile(app, "${app.packageName}.files", oversized)
            compose.runOnIdle {
                app.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newUri(app.contentResolver, "oversized", uri))
            }
            compose.onNode(hasSetTextAction()).performSemanticsAction(SemanticsActions.PasteText)
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Trop lourd", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithText(long).performClick()
            compose.onNodeWithText("Modifier").performClick()
            compose.runOnIdle {
                val draft = checkNotNull(vm.chatDrafts["chat"])
                assertNull(draft.editing)
                assertEquals("", draft.text)
                assertEquals("queue-oversized.png", draft.rejected.single().name)
            }
            compose.onNodeWithContentDescription("Retirer queue-oversized.png").performClick()

            compose.onNodeWithText(long).performClick()
            compose.onNodeWithTag("queued-message-sheet").assertExists()
            listOf(
                    "Envoyer maintenant",
                    "Modifier",
                    "Mettre la file en pause",
                    "Retirer de la file",
                )
                .forEach {
                    compose.onNodeWithText(it).assertExists()
                }
            compose.onNodeWithText("L’agent le lit sans attendre la fin de sa tâche").assertExists()
            compose.waitForIdle()
            capture("queue-sheet")
            compose.onNodeWithText("Mettre la file en pause").performClick()
            compose.waitUntil(10000) {
                calls.any { it.second == "/api/installations/fixture/api/chats/chat/pause" }
            }
            assertTrue(
                wireJson
                    .parseToJsonElement(calls.first { it.second.endsWith("/pause") }.third)
                    .jsonObject
                    .getValue("paused")
                    .jsonPrimitive
                    .boolean
            )
            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("queued-message-sheet").fetchSemanticsNodes().isEmpty()
            }
            settle()

            compose.onNodeWithText(long).performClick()
            compose.onNodeWithText("Retirer de la file").performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Retirer ce message ?")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertTrue("Removal must be confirmed", calls.none { it.first == "DELETE" })
            compose.onNodeWithText("Annuler").performClick()

            compose.waitUntil(10000) {
                compose.onAllNodesWithTag("chat-page:neighbour").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onAllNodesWithTag("queued-message").onFirst().performTouchInput { swipeLeft() }
            compose.onNodeWithText("Retirer", substring = false).assertIsDisplayed()
            compose.onNodeWithTag("chat-page:chat").assert(isSelected())
            assertTrue("A swipe only reveals removal", calls.none { it.first == "DELETE" })
            compose.onNodeWithText("Retirer", substring = false).performClick()
            compose.onNodeWithText("Confirmer").performClick()
            compose.waitUntil(10000) {
                calls.any {
                    it.first == "DELETE" &&
                        it.second == "/api/installations/fixture/api/chats/chat/messages/first"
                }
            }
            vm.api.closeStreams()
        }
    }
}

private class QueueVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
