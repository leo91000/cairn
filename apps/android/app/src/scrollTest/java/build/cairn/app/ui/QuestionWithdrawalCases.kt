package build.cairn.app.ui

import android.app.Application
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** The same native withdrawal interaction runs on the JVM and an Android device. */
abstract class QuestionWithdrawalCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun withdrawalRequiresNoAnswerAndCanBeRetriedAfterARejectedRequest() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<Pair<String, String>>()
            val question =
                ChatQuestion(
                    "question",
                    "chat",
                    blocking = true,
                    fields = listOf(QuestionField("scope", "Quelle portée ?")),
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (request.method == "POST" && path.endsWith("/cancel")) {
                            writes += path to request.body.readUtf8()
                            if (writes.size == 1)
                                return MockResponse()
                                    .setResponseCode(409)
                                    .setBody("""{"error":"Réessayez le retrait."}""")
                            return MockResponse().setBody("""{"status":"cancelled"}""")
                        }
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
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
                                    else if (
                                        request.requestUrl?.encodedPath?.endsWith("/api/mcps") ==
                                            true
                                    )
                                        """{"mcps":[]}"""
                                    else "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var closed by mutableStateOf(false)
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme {
                    if (state.installation != null) {
                        if (closed) Text("Question retirée")
                        else
                            QuestionDialog(vm, state, question, listOf(question), {}) {
                                closed = true
                            }
                    }
                }
            }
            fun ready() =
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Ce n’est plus utile")
                        .fetchSemanticsNodes()
                        .isNotEmpty() && !vm.state.value.busy
                }
            try {
                ready()
                compose.onNodeWithText("Envoyer la réponse").assertIsNotEnabled()
                compose.onNodeWithText("Ce n’est plus utile").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Réessayez le retrait.")
                        .fetchSemanticsNodes()
                        .isNotEmpty() && !vm.state.value.busy
                }
                assertFalse(closed)
                assertEquals(1, writes.size)
                compose.onNodeWithText("Ce n’est plus utile").performClick()
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithText("Question retirée")
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                assertEquals(2, writes.size)
                assertTrue(
                    writes.all {
                        it.first ==
                            "/api/installations/fixture/api/chats/chat/questions/question/cancel"
                    }
                )
                assertTrue(writes.all { !it.second.contains("answers") })
                compose.onNodeWithText("Ce n’est plus utile").assertDoesNotExist()
            } finally {
                models.clear()
            }
        }
    }

    @Test
    fun multipleChoicesKeepTheirCommentAndRespectTheSelectionBounds() {
        MockWebServer().use { server ->
            val writes = CopyOnWriteArrayList<JsonObject>()
            val question =
                ChatQuestion(
                    "question",
                    "chat",
                    fields =
                        listOf(
                            QuestionField(
                                "platforms",
                                "Quelles plateformes ?",
                                mode = "multiple",
                                minChoices = 2,
                                maxChoices = 2,
                                options =
                                    listOf(
                                        QuestionOption(
                                            "Android",
                                            id = "android",
                                            recommended = true,
                                        ),
                                        QuestionOption("Web", id = "web"),
                                        QuestionOption("Desktop", id = "desktop"),
                                    ),
                            )
                        ),
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (request.method == "POST" && path.endsWith("/answer")) {
                            writes +=
                                wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                            return MockResponse().setBody("{}")
                        }
                        val body =
                            when {
                                path == "/api/account/session" -> beaconAccountFixture("fixture")
                                path == "/api/installations" -> beaconInstallationsFixture()
                                path.endsWith("/overview") || path.endsWith("/models") -> "{}"
                                path.endsWith("/projects") -> """{"projects":[]}"""
                                path.endsWith("/agents") -> """{"agents":[]}"""
                                path.endsWith("/tasks") -> """{"tasks":[]}"""
                                path.endsWith("/mcps") -> """{"mcps":[]}"""
                                else -> "[]"
                            }
                        return MockResponse()
                            .setHeader("Content-Type", "application/json")
                            .setBody(body)
                    }
                }
            val origin = server.url("/").toString()
            val vm =
                CairnViewModel(
                    ApplicationProvider.getApplicationContext<Application>(),
                    object : SessionVault {
                        override fun read(origin: String): String? = null

                        override fun write(origin: String, cookie: String?) {}
                    },
                    beaconOrigin = "",
                )
            val models = ViewModelStore().apply { put("fixture", vm) }
            var closed by mutableStateOf(false)
            compose.setContent {
                val state by vm.state.collectAsState()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(origin)
                }
                CairnTheme {
                    if (state.installation != null) {
                        if (closed) Text("Réponse envoyée")
                        else
                            QuestionDialog(vm, state, question, listOf(question), {}) {
                                closed = true
                            }
                    }
                }
            }
            try {
                compose.waitUntil(20000) {
                    compose
                        .onAllNodesWithText("Envoyer la réponse")
                        .fetchSemanticsNodes()
                        .isNotEmpty() && !vm.state.value.busy
                }
                compose.onNodeWithText("Envoyer la réponse").assertIsNotEnabled()
                compose.onNodeWithText("Recommandé").assertExists()
                compose.onNodeWithText("Android", substring = true).performClick()
                compose
                    .onNode(hasText("Commentaire ou réponse libre") and hasSetTextAction())
                    .performTextInput("Android en priorité")
                compose.onNodeWithText("Envoyer la réponse").assertIsNotEnabled()
                compose.onNodeWithText("Web").performClick()
                compose.onNodeWithText("Envoyer la réponse").assertIsEnabled()
                compose.onNodeWithText("Desktop").performClick()
                compose.onNodeWithText("Envoyer la réponse").assertIsNotEnabled()
                compose.onNodeWithText("Desktop").performClick()
                compose
                    .onNodeWithText("Envoyer la réponse")
                    .assertIsEnabled()
                    .assertIsDisplayed()
                    .performClick()
                try {
                    compose.waitUntil(10000) {
                        compose
                            .onAllNodesWithText("Réponse envoyée")
                            .fetchSemanticsNodes()
                            .isNotEmpty() || vm.state.value.error != null
                    }
                } catch (error: androidx.compose.ui.test.ComposeTimeoutException) {
                    throw AssertionError(
                        "writes=${writes.size}, closed=$closed, busy=${vm.state.value.busy}, error=${vm.state.value.error}",
                        error,
                    )
                }
                assertEquals("Submission failed: ${vm.state.value.error}", 1, writes.size)
                assertTrue("Submission did not close: ${vm.state.value.error}", closed)
                val answer = writes.single()["answers"]!!.jsonObject["platforms"]!!.jsonObject
                assertEquals(
                    listOf("android", "web"),
                    answer["selected"]!!.jsonArray.map { it.jsonPrimitive.content },
                )
                assertEquals("Android en priorité", answer["comment"]!!.jsonPrimitive.content)
            } finally {
                models.clear()
            }
        }
    }
}
