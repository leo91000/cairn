package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

private const val CHAT_ID = "a1b2c3d4-0000-4000-8000-0000000000c1"

/** The same native side-question interactions run on the JVM and an Android device. */
abstract class SideQuestionCases {
    @get:Rule val compose = createComposeRule()

    private val answered =
        ChatSideQuestion(
            id = "a1b2c3d4-0000-4000-8000-000000000001",
            chatId = CHAT_ID,
            runId = "run",
            text = "Pourquoi cet ordre ?",
            itemId = "reply-1",
            provider = "claude",
            status = "answered",
            answer = "Parce que la migration doit précéder le code.",
            createdAt = 1,
            settledAt = 2,
        )
    private val pending =
        ChatSideQuestion(
            id = "a1b2c3d4-0000-4000-8000-000000000002",
            chatId = CHAT_ID,
            runId = "run",
            text = "Et les tests ?",
            itemId = "reply-2",
            provider = "claude",
            status = "pending",
            createdAt = 3,
            settledAt = null,
        )

    private fun chat(canAsk: Boolean, provider: String = "claude") =
        Chat(
            id = CHAT_ID,
            provider = provider,
            status = "running",
            canAskSideQuestion = canAsk,
            sideQuestions = listOf(answered, pending),
        )

    private fun server(writes: MutableList<Pair<String, String>>) =
        MockWebServer().apply {
            dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (request.method == "POST" && path.contains("/side-questions")) {
                            writes += path to request.body.readUtf8()
                            val question =
                                if (path.endsWith("/cancel"))
                                    pending.copy(status = "cancelled", settledAt = 4)
                                else pending
                            return MockResponse()
                                .setHeader("Content-Type", "application/json")
                                .setBody(Json.encodeToString(question))
                        }
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                path.endsWith("/api/projects") -> """{"projects":[]}"""
                                path.endsWith("/api/agents") -> """{"agents":[]}"""
                                path.endsWith("/api/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/api/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
        }

    private fun viewModel() =
        CairnViewModel(
            ApplicationProvider.getApplicationContext<Application>(),
            object : SessionVault {
                override fun read(origin: String): String? = null

                override fun write(origin: String, cookie: String?) {}
            },
            beaconOrigin = "",
        )

    @Test
    fun aQuotedSideQuestionIsSentWithItsReplyAndAPendingOneCanBeWithdrawn() {
        val writes = CopyOnWriteArrayList<Pair<String, String>>()
        server(writes).use { server ->
            val vm = viewModel()
            val models = ViewModelStore().apply { put("fixture", vm) }
            val side = SideQuestionState().apply { ask("reply-1", "  la migration d’abord  ") }
            val revealed = mutableListOf<String>()
            // Asking clears the passage from the form.
            fun cleared() =
                compose.onAllNodesWithText("la migration d’abord").fetchSemanticsNodes().isEmpty()
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.installation != null && side.open)
                        Column(Modifier.verticalScroll(rememberScrollState())) {
                            SideQuestionPanel(vm, state, chat(canAsk = true), side) {
                                revealed += it
                            }
                        }
                }
            }
            try {
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Votre question sur ce passage")
                        .fetchSemanticsNodes()
                        .isNotEmpty() && !vm.state.value.busy
                }
                compose.onNodeWithText("la migration d’abord").assertExists()
                // The answer renders as Markdown, like replies.
                compose.onAllNodesWithTag("side-question-answer").assertCountEquals(1)
                compose.onNodeWithText("Réponse en cours…").assertExists()
                compose
                    .onNode(hasSetTextAction() and hasText("Votre question sur ce passage"))
                    .performScrollTo()
                    .performTextInput("Pourquoi ?")
                compose
                    .onNodeWithText("Demander")
                    .performScrollTo()
                    .assertIsEnabled()
                    .performClick()
                // Reading the UI drains the main looper, where the request completes.
                compose.waitUntil(10000) { cleared() && writes.size == 1 && !vm.state.value.busy }
                assertEquals(
                    "/api/installations/fixture/api/chats/$CHAT_ID/side-questions",
                    writes[0].first,
                )
                val body = Json.parseToJsonElement(writes[0].second).jsonObject
                assertEquals("Pourquoi ?", body["text"]!!.jsonPrimitive.content)
                assertEquals("la migration d’abord", body["quote"]!!.jsonPrimitive.content)
                assertEquals("reply-1", body["itemId"]!!.jsonPrimitive.content)
                assertEquals(36, body["id"]!!.jsonPrimitive.content.length)
                // The form clears once asked; the passage is no longer quoted.
                compose.onNodeWithText("la migration d’abord").assertDoesNotExist()

                compose.onNodeWithText("Retirer").performScrollTo().performClick()
                compose.waitUntil(10000) { cleared() && writes.size == 2 && !vm.state.value.busy }
                assertEquals(
                    "/api/installations/fixture/api/chats/$CHAT_ID/side-questions/${pending.id}/cancel",
                    writes[1].first,
                )
                compose.onAllNodesWithText("Voir la réponse")[1].performScrollTo().performClick()
                compose.waitUntil(5000) { cleared() && revealed.isNotEmpty() }
                assertEquals(listOf("reply-1"), revealed)
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun anEngineThatCannotAnswerExplainsWhyWithoutAForm() {
        val writes = CopyOnWriteArrayList<Pair<String, String>>()
        server(writes).use { server ->
            val vm = viewModel()
            val models = ViewModelStore().apply { put("fixture", vm) }
            val side = SideQuestionState().apply { show("reply-2") }
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.installation != null)
                        Column(Modifier.verticalScroll(rememberScrollState())) {
                            SideQuestionPanel(vm, state, chat(canAsk = false, "codex"), side) {}
                        }
                }
            }
            try {
                compose.waitUntil(20000) {
                    compose.onAllNodesWithText("Tout afficher").fetchSemanticsNodes().isNotEmpty()
                }
                compose
                    .onNodeWithText(
                        "Codex ne peut pas répondre aux questions à côté dans Cairn : ses conversations à côté peuvent encore utiliser des outils. Posez-la dans la conversation."
                    )
                    .assertExists()
                compose.onNodeWithText("Demander").assertDoesNotExist()
                // Focused on one reply, only its question shows until the user asks for all.
                compose.onNodeWithText("Et les tests ?").assertExists()
                compose.onNodeWithText("Pourquoi cet ordre ?").assertDoesNotExist()
                compose.onNodeWithText("Tout afficher").performClick()
                compose.onNodeWithText("Pourquoi cet ordre ?").assertExists()
                assertTrue(writes.isEmpty())
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun aReplyOffersItsSideQuestionsOnlyWhileTheAgentCanAnswer() {
        val asked = mutableListOf<Pair<String, String>>()
        val shown = mutableListOf<String>()
        var canAsk by mutableStateOf(true)
        val vm = viewModel()
        val models = ViewModelStore().apply { put("fixture", vm) }
        val event =
            RunEvent(
                id = 1,
                createdAt = 0,
                type = "item.completed",
                text = "Réponse",
                payload =
                    buildJsonObject {
                        put(
                            "item",
                            buildJsonObject {
                                put("id", "reply-1")
                                put("type", "agent_message")
                                put("text", "Réponse de l’agent")
                            },
                        )
                    },
                runId = "run",
            )
        compose.setContent {
            CairnTheme {
                CompositionLocalProvider(
                    LocalSideQuestions provides
                        SideQuestionActions(
                            canAsk,
                            mapOf("reply-1" to 2),
                            { item, quote -> asked += item to quote },
                            { shown += it },
                        )
                ) {
                    EventRow(vm, event, "Agent")
                }
            }
        }
        try {
            compose.onNodeWithContentDescription("Poser une question à côté").performClick()
            assertEquals(listOf("reply-1" to ""), asked)
            compose.onNodeWithText("2").performClick()
            assertEquals(listOf("reply-1"), shown)
            val label = compose.onNodeWithText("Agent").getBoundsInRoot()
            canAsk = false
            compose.waitForIdle()
            // The action leaving does not move the reply, so the history keeps its position.
            val after = compose.onNodeWithText("Agent").getBoundsInRoot()
            assertEquals(label.top to label.bottom, after.top to after.bottom)
            compose.onNodeWithContentDescription("Poser une question à côté").assertDoesNotExist()
            compose.onNodeWithText("2").assertExists()
        } finally {
            models.clear()
        }
    }
}
