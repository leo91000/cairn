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
}
