package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
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
class ContextCompactionJourneyTest {
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

    private fun item(vararg fields: Pair<String, Any>) = buildJsonObject {
        for ((key, value) in fields) when (value) {
            is Number -> put(key, value)
            else -> put(key, value.toString())
        }
    }

    @Test
    fun `context usage and a compaction show natively and compaction is requested explicitly`() {
        MockWebServer().use { server ->
            val chatId = "00000000-0000-4000-8000-000000000099"
            val calls = CopyOnWriteArrayList<Pair<String, String>>()
            val chat =
                Chat(
                    chatId,
                    title = "Refonte du module",
                    agentName = "Cairn",
                    status = "succeeded",
                    provider = "claude",
                    runId = "run",
                    run = Run("run", status = "succeeded", trigger = "chat"),
                    contextUsage = ContextUsage(150_000, 200_000, 120_000, false),
                    compaction = ChatCompaction(supported = true, available = true),
                )
            val events =
                listOf(
                    RunEvent(1, 1789315189000, "chat.user", "Refactorise le module de paiement"),
                    RunEvent(
                        2,
                        1789315190000,
                        "context.usage",
                        "",
                        mapOf("usage" to item("contextTokens" to 150_000)),
                    ),
                    RunEvent(
                        3,
                        1789315190100,
                        "item.started",
                        "",
                        mapOf(
                            "item" to
                                item(
                                    "id" to "compaction-1",
                                    "type" to "context_compaction",
                                    "status" to "in_progress",
                                )
                        ),
                    ),
                    RunEvent(
                        4,
                        1789315190200,
                        "item.completed",
                        "",
                        mapOf(
                            "item" to
                                item(
                                    "id" to "compaction-1",
                                    "type" to "context_compaction",
                                    "status" to "completed",
                                    "trigger" to "auto",
                                    "pre_tokens" to 167_500,
                                    "post_tokens" to 9_100,
                                )
                        ),
                    ),
                    RunEvent(
                        5,
                        1789315190300,
                        "item.completed",
                        "",
                        mapOf(
                            "item" to
                                item(
                                    "id" to "answer",
                                    "type" to "agent_message",
                                    "text" to "Le module est refactorisé.",
                                )
                        ),
                    ),
                )
            fun stream(state: LiveState, events: List<RunEvent> = emptyList()): MockResponse {
                val body =
                    "event: batch\nid: ${events.lastOrNull()?.id ?: 0}\ndata: ${wireJson.encodeToString(LiveBatch(events, state, false, false))}\n\n"
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
                        if (method in listOf("POST", "PUT", "DELETE")) calls += method to path
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
                                "/api/installations/fixture/api/chats/stream" ->
                                    return stream(LiveState(chats = listOf(chat)))
                                "/api/installations/fixture/api/chats/$chatId/stream" ->
                                    return stream(LiveState(chat = chat, run = chat.run), events)
                                "/api/installations/fixture/api/chats/$chatId/compact" ->
                                    wireJson.encodeToString(
                                        chat.copy(
                                            compaction =
                                                ChatCompaction(supported = true, running = true)
                                        )
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
            compose.setContent {
                val workspace by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    vm.connect(server.url("/").toString())
                }
                if (workspace.session.authenticated)
                    CairnTheme("light") {
                        Surface(Modifier.fillMaxWidth().height(915.dp)) {
                            ChatScreen(vm, workspace, chatId, openChat = {}, openRun = {})
                        }
                    }
            }
            try {
                compose.waitUntil(10000) {
                    compose
                        .onAllNodesWithTag("context-compaction", useUnmergedTree = true)
                        .fetchSemanticsNodes()
                        .isNotEmpty()
                }
                // The compaction shows where it happened, without hiding the request before it.
                val divider = compose.onNodeWithTag("context-compaction", useUnmergedTree = true)
                divider.assertExists()
                compose
                    .onNode(
                        hasContentDescription(
                            "Contexte compacté automatiquement. 168 k → 9 k jetons · Les messages précédents restent visibles"
                        )
                    )
                    .assertExists()
                compose
                    .onAllNodesWithText("Refactorise le module de paiement")
                    .onFirst()
                    .assertExists()
                screenshot("context-compaction")

                compose.onNodeWithContentDescription("Options de la conversation").performClick()
                compose.onNodeWithText("Détails de la conversation").performClick()
                waitText("Contexte : 150 000 sur 200 000 jetons (75 %)")
                compose.onNodeWithText("120 000 jetons lus depuis le cache").assertExists()
                compose.onNodeWithText("Mesuré sur la dernière requête du modèle").assertExists()
                screenshot("context-details")
                compose.onNodeWithText("Compacter le contexte").assertIsEnabled().performClick()
                waitText("Compacter le contexte de l’agent ?")
                screenshot("context-compaction-confirm", dialog = true)
                assertTrue(calls.none { it.second.endsWith("/compact") })
                compose.onNodeWithText("Confirmer").performClick()
                compose.waitUntil(10000) { calls.any { it.second.endsWith("/compact") } }
                assertEquals(
                    "POST" to "/api/installations/fixture/api/chats/$chatId/compact",
                    calls.single { it.second.endsWith("/compact") },
                )
                // Compaction is never sent as a chat message.
                assertTrue(calls.none { it.second.endsWith("/messages") })
            } finally {
                vm.api.closeStreams()
            }
        }
    }

    private fun waitText(text: String) {
        compose.waitUntil(10000) {
            compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty()
        }
    }

    private fun screenshot(name: String, dialog: Boolean = false) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
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
