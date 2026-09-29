package dev.leo.manager.ui

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import dev.leo.manager.data.*
import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.*
import org.junit.Assert.*

abstract class ImagePasteCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun pastedImageJoinsTheAttachmentsAndIsSentWithTheMessage() {
        MockWebServer().use { server ->
            val app = ApplicationProvider.getApplicationContext<Application>()
            val uploads = CopyOnWriteArrayList<Pair<String, Long>>()
            val sent = CopyOnWriteArrayList<JsonObject>()
            val agent = Agent(MAIN_AGENT_ID, "Agent principal")
            val run = Run("run", status = "succeeded", snapshot = Snapshot(agent = agent))
            val chat =
                Chat("chat", title = "Capture", runId = "run", agentName = agent.name, run = run)
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        if (path == "/api/chats/chat/stream") {
                            val frame =
                                "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(emptyList(), LiveState(chat = chat, run = run), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    java.util.concurrent.TimeUnit.SECONDS,
                                )
                        }
                        if (
                            request.method == "PUT" &&
                                path.startsWith("/api/chats/chat/attachments/")
                        ) {
                            val id = path.substringAfterLast('/')
                            val name = request.requestUrl!!.queryParameter("name")!!
                            uploads += name to request.bodySize
                            return MockResponse()
                                .setBody(
                                    wireJson.encodeToString(
                                        ChatAttachment(
                                            id,
                                            "chat",
                                            name,
                                            request.bodySize,
                                            "image/png",
                                            "image",
                                        )
                                    )
                                )
                        }
                        val body =
                            when (path) {
                                "/api/session" -> """{"authenticated":true,"csrf":"fixture"}"""
                                "/api/agents" -> wireJson.encodeToString(listOf(agent))
                                "/api/chats/chat/messages" -> {
                                    sent +=
                                        wireJson
                                            .parseToJsonElement(request.body.readUtf8())
                                            .jsonObject
                                    "{}"
                                }
                                "/api/codex/models" -> """{"models":[]}"""
                                "/api/overview" -> "{}"
                                else -> "[]"
                            }
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val image =
                File(app.cacheDir, "leo-files/capture.png").apply {
                    parentFile!!.mkdirs()
                    writeBytes(ByteArray(2048) { it.toByte() })
                }
            val uri = FileProvider.getUriForFile(app, "${app.packageName}.files", image)
            val vm = LeoViewModel(app, PasteVault())
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                LeoTheme {
                    if (state.session.authenticated)
                        ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
                }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText("Votre message…").fetchSemanticsNodes().isNotEmpty()
            }
            val field = compose.onNode(hasSetTextAction())
            field.performTextInput("Regarde cette capture")
            compose.runOnIdle {
                app.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newUri(app.contentResolver, "Capture", uri))
            }
            field.performSemanticsAction(SemanticsActions.PasteText)
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasText("capture.png", substring = true))
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            // The image becomes an attachment; the text stays as typed.
            assertEquals(
                "Regarde cette capture",
                field
                    .fetchSemanticsNode()
                    .config[androidx.compose.ui.semantics.SemanticsProperties.EditableText]
                    .text,
            )
            compose.onNodeWithContentDescription("Retirer capture.png").assertExists()
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").performClick()
            // The composer empties once the message is accepted; querying it keeps the UI loop
            // running.
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Votre message…").fetchSemanticsNodes().isNotEmpty()
            }
            assertEquals(1, sent.size)
            assertEquals(listOf("capture.png" to 2048L), uploads.toList())
            assertEquals("Regarde cette capture", sent[0]["text"]?.jsonPrimitive?.content)
            assertEquals(1, sent[0]["attachmentIds"]?.jsonArray?.size)
            // Plain text still pastes into the message.
            compose.runOnIdle {
                app.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newPlainText("Texte", "Texte collé"))
            }
            field.performSemanticsAction(SemanticsActions.PasteText)
            compose.waitUntil(15000) {
                compose
                    .onAllNodes(hasText("Texte collé") and hasSetTextAction())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Retirer capture.png").assertDoesNotExist()
        }
    }
}

private class PasteVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
