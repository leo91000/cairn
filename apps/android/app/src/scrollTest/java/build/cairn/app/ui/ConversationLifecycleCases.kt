package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class ConversationLifecycleCases {
    @get:Rule val compose = createComposeRule()

    protected open fun captureSwipe() = Unit

    @Test fun swipeRequiresConfirmationAndPermanentlyDeletes() = withConversation(false)

    @Test fun swipeFromFilConfirmsBeforePermanentlyDeleting() = withConversation(true)

    @Test
    fun deletingWorkingConversationFromFilRequiresConfirmation() = withConversation(true, true)

    private fun withConversation(fromFil: Boolean, working: Boolean = false) {
        MockWebServer().use { server ->
            val deleted = AtomicBoolean(false)
            val deletes = CopyOnWriteArrayList<String>()
            val restores = CopyOnWriteArrayList<String>()
            val chat =
                Chat(
                    "chat",
                    title = "Conversation à conserver",
                    agentName = "Agent principal",
                    updatedAt = System.currentTimeMillis(),
                    status = if (working) "running" else "succeeded",
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/installations/fixture/api/chats/stream") {
                            val chats = if (deleted.get()) emptyList() else listOf(chat)
                            val frame =
                                "event: batch\nid: 0\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(chats = chats), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame)
                        }
                        if (path == "/api/installations/fixture/api/chats/chat/stream") {
                            val current =
                                chat.copy(
                                    lifecycle = if (deleted.get()) "trash" else "active",
                                    purgeAt =
                                        if (deleted.get()) System.currentTimeMillis() + 86400000
                                        else null,
                                )
                            val frame =
                                "event: batch\nid: 0\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(chat = current), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame)
                        }
                        val body =
                            when {
                                path == "/api/installations/fixture/api/chats/chat/restore" &&
                                    request.method == "POST" -> {
                                    restores += request.body.readUtf8()
                                    deleted.set(false)
                                    wireJson.encodeToString(chat)
                                }
                                path == "/api/installations/fixture/api/chats/chat" &&
                                    request.method == "DELETE" -> {
                                    val payload = request.body.readUtf8()
                                    deletes += payload
                                    if (
                                        working &&
                                            !wireJson
                                                .parseToJsonElement(payload)
                                                .jsonObject
                                                .getValue("confirm")
                                                .jsonPrimitive
                                                .boolean
                                    )
                                        return MockResponse()
                                            .setResponseCode(409)
                                            .setBody("""{"error":"Confirmation requise"}""")
                                    deleted.set(true)
                                    "{}"
                                }
                                path == "/api/installations/fixture/api/chats" &&
                                    request.path!!.contains("view=trash") -> {
                                    val trashed =
                                        wireJson.encodeToJsonElement(chat).jsonObject.toMutableMap()
                                    trashed["lifecycle"] = JsonPrimitive("trash")
                                    JsonArray(listOf(JsonObject(trashed))).toString()
                                }
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        listOf(Agent(MAIN_AGENT_ID, "Agent principal"))
                                    )
                                path == "/api/installations/fixture/api/overview" -> "{}"
                                path.endsWith("/models") -> """{"models":[]}"""
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    LifecycleVault(),
                    beaconOrigin = "",
                )
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                var selected by remember { mutableStateOf<String?>(null) }
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (fromFil) CairnApp(vm = vm)
                    else if (state.session.authenticated) {
                        if (selected == null)
                            ChatsScreen(vm, state, open = { selected = it }, create = {})
                        else
                            ChatScreen(
                                vm,
                                state,
                                selected,
                                openChat = { selected = it },
                                openRun = {},
                                back = { selected = null },
                            )
                    }
                }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText(chat.title).fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Corbeille").assertDoesNotExist()
            val row = compose.onNodeWithText(chat.title)
            // Swiping right, or releasing a slow drag before the threshold, springs the row back.
            row.performTouchInput { swipeRight() }
            row.performTouchInput {
                swipe(Offset(width * .9f, centerY), Offset(width * .7f, centerY), 800)
            }
            compose.waitForIdle()
            row.assertIsDisplayed()
            assertTrue("A short drag must not delete", deletes.isEmpty())
            row.performTouchInput { swipeLeft() }
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Supprimer définitivement ?")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            assertTrue("No request is sent before confirmation", deletes.isEmpty())
            compose.onNodeWithText("Annuler", substring = false).performClick()
            compose.onNodeWithText(chat.title).assertIsDisplayed()
            assertFalse(deleted.get())
            compose.onNodeWithText(chat.title).performTouchInput {
                down(Offset(width * .9f, centerY))
                moveTo(Offset(width * .3f, centerY))
            }
            if (fromFil) {
                compose.waitForIdle()
                captureSwipe()
            }
            row.performTouchInput { up() }
            compose.waitUntil(10000) {
                compose.onAllNodesWithText("Confirmer").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("Confirmer", substring = false).performClick()
            compose.waitUntil(10000) { deletes.size == 1 }
            assertTrue(
                wireJson
                    .parseToJsonElement(deletes.single())
                    .jsonObject
                    .getValue("confirm")
                    .jsonPrimitive
                    .boolean
            )
            assertTrue("Permanent deletion must never restore", restores.isEmpty())
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Annuler", substring = false)
                    .fetchSemanticsNodes()
                    .isEmpty()
            }
            compose.onNodeWithText("Annuler", substring = false).assertDoesNotExist()
            compose.waitUntil(15000) {
                compose.onAllNodesWithText(chat.title).fetchSemanticsNodes().isEmpty()
            }
            if (fromFil) compose.onNodeWithTag("fil").assertIsDisplayed()
            vm.api.closeStreams()
        }
    }
}

private class LifecycleVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
