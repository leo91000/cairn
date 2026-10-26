package build.cairn.app.ui

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import androidx.compose.runtime.*
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.RandomAccessFile
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import okio.Buffer
import org.junit.*
import org.junit.Assert.*

/**
 * A sent message with pictures, documents and an archive: the mosaic and cards load server previews
 * and summaries, the viewer pages through the files, a long press attaches one again and the ＋
 * button opens the attach sheet.
 */
abstract class AttachmentCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) {}

    private fun picture(color: Int, page: Boolean = false): ByteArray {
        val bitmap =
            Bitmap.createBitmap(
                if (page) 300 else 320,
                if (page) 400 else 240,
                Bitmap.Config.ARGB_8888,
            )
        val canvas = Canvas(bitmap)
        canvas.drawColor(color)
        if (page) {
            val line = android.graphics.Paint().apply { this.color = 0xFFDDD8CC.toInt() }
            for (row in 0 until 12) canvas.drawRect(
                30f,
                60f + row * 26f,
                270f - (row % 3) * 50f,
                72f + row * 26f,
                line,
            )
        }
        return ByteArrayOutputStream()
            .also { bitmap.compress(Bitmap.CompressFormat.JPEG, 90, it) }
            .toByteArray()
    }

    @Test
    fun sentFilesShowPreviewsOpenInTheViewerAndCanBeAttachedAgain() {
        MockWebServer().use { server ->
            val app = ApplicationProvider.getApplicationContext<Application>()
            val requested = CopyOnWriteArrayList<String>()
            val agent =
                Agent(
                    exclusiveNodes = emptyList(),
                    plugins = emptyList(),
                    revision = "",
                    createdAt = 0,
                    access =
                        AccessPolicy(
                            mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                            projects = null,
                            skills = null,
                            mcps = null,
                        ),
                    id = MAIN_AGENT_ID,
                    name = "Agent principal",
                )
            val run =
                Run(
                    id = "run",
                    status = "succeeded",
                    snapshot =
                        Snapshot(
                            agent = agent,
                            task =
                                build.cairn.contracts.RunTaskSnapshot(
                                    agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                    id = "00000000-0000-4000-8000-000000000003",
                                    name = "Fixture",
                                    prompt = "Fixture",
                                ),
                        ),
                    taskId = "",
                )
            val chat =
                Chat(
                    "chat",
                    title = "Refonte page tarifs",
                    runId = "run",
                    agentName = agent.name,
                    run = run,
                )
            val files =
                listOf(
                    ChatAttachment(
                        "shot",
                        "chat",
                        "Screenshot_20261017-142133_Chrome.png",
                        1_900_000,
                        "image/png",
                        "image",
                    ),
                    ChatAttachment(
                        "hdr",
                        "chat",
                        "IMG_20261017_141502_HDR.jpg",
                        3_400_000,
                        "image/jpeg",
                        "image",
                    ),
                    ChatAttachment(
                        "park",
                        "chat",
                        "IMG_20261017_141733.jpg",
                        2_100_000,
                        "image/jpeg",
                        "image",
                    ),
                    ChatAttachment(
                        "brief",
                        "chat",
                        "Brief_refonte_page_tarifs_v3_FINAL_relu.pdf",
                        831_488,
                    ),
                    ChatAttachment("export", "chat", "export_clients_2026-10-17.csv", 253_952),
                    ChatAttachment("sources", "chat", "maquettes-sources.zip", 6_200_000),
                )
            val pictures =
                mapOf(
                    "shot" to picture(0xFFE98B6D.toInt()),
                    "hdr" to picture(0xFF4F8F93.toInt()),
                    "park" to picture(0xFFA8C8EC.toInt()),
                    "brief" to picture(Color.WHITE, page = true),
                )
            val message = "Voilà la maquette actuelle, deux photos et le brief"
            val events =
                listOf(
                    RunEvent(
                        1,
                        System.currentTimeMillis() - 60_000,
                        "chat.user",
                        message,
                        mapOf(
                            "text" to JsonPrimitive(message),
                            "attachments" to wireJson.encodeToJsonElement(files),
                        ),
                        runId = "run",
                    )
                )
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        val path = request.path!!.substringBefore('?')
                        val query = request.path!!.substringAfter('?', "")
                        val prefix = "/api/installations/fixture/api/chats/chat/attachments/"
                        if (path == "/api/installations/fixture/api/chats/chat/stream") {
                            val frame =
                                "event: batch\nid: 1\ndata: ${wireJson.encodeToString(LiveBatch(events, LiveState(chat = chat, run = run), true, false))}\n\n"
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame + ": keepalive\n\n".repeat(10000))
                                .throttleBody(
                                    frame.toByteArray().size.toLong(),
                                    1,
                                    java.util.concurrent.TimeUnit.SECONDS,
                                )
                        }
                        if (path.startsWith(prefix)) {
                            val id = path.removePrefix(prefix).removeSuffix("/stream")
                            requested += "${path.removePrefix(prefix)}?$query"
                            val picture = pictures[id]
                            return when {
                                query == "summary" ->
                                    MockResponse()
                                        .setBody(
                                            when (id) {
                                                "brief" -> """{"pages":12}"""
                                                "export" ->
                                                    """{"lines":1204,"excerpt":"id,nom,plan,mrr\n1042,Acme,pro,490\n1043,Globex,team,1290\n1044,Initech,pro,490"}"""
                                                else -> "{}"
                                            }
                                        )
                                picture != null ->
                                    MockResponse()
                                        .setHeader("Content-Type", "image/jpeg")
                                        .setBody(Buffer().write(picture))
                                        .setBodyDelay(
                                            if (query.isEmpty()) 300 else 0,
                                            java.util.concurrent.TimeUnit.MILLISECONDS,
                                        )
                                query == "preview" ->
                                    MockResponse().setResponseCode(404).setBody("{}")
                                else -> MockResponse().setBody("id,nom\n1042,Acme\n")
                            }
                        }
                        val body =
                            when (path) {
                                "/api/installations" -> beaconInstallationsFixture()
                                "/api/account/session" -> beaconAccountFixture("fixture")
                                "/api/installations/fixture/api/agents" ->
                                    wireJson.encodeToString(
                                        build.cairn.contracts.ListAgentsOutput(listOf(agent))
                                    )
                                "/api/installations/fixture/api/codex/models" -> """{"models":[]}"""
                                "/api/installations/fixture/api/overview" -> "{}"
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
                        return MockResponse().setBody(body)
                    }
                }
            server.start()
            val vm = CairnViewModel(app, AttachmentVault(), beaconOrigin = "")
            var show by mutableStateOf(true)
            compose.setContent {
                val state by vm.state.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) {
                    vm.state.first { it.ready }
                    if (!vm.state.value.session.authenticated)
                        vm.connect(server.url("/").toString())
                }
                CairnTheme {
                    if (state.session.authenticated && show)
                        ChatScreen(vm, state, "chat", openChat = {}, openRun = {})
                }
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithText(message).fetchSemanticsNodes().isNotEmpty()
            }

            // Pictures form a mosaic, the PDF and the CSV picture their content, the archive is a
            // one-line card; counts come from the server summary.
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("12 pages").fetchSemanticsNodes().isNotEmpty() &&
                    compose.onAllNodesWithText("1 204 lignes").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithContentDescription("IMG_20261017_141733.jpg").assertIsDisplayed()
            compose.onNodeWithText("ZIP · 5,9 Mo").assertIsDisplayed()
            assertTrue("brief?preview" in requested)
            assertTrue("shot?preview" in requested)
            assertFalse("sources?summary" in requested)
            capture("01-conversation")

            // Dismissing the action sheet must not cancel a cold-cache original download.
            val clipboard = app.getSystemService(ClipboardManager::class.java)
            clipboard.clearPrimaryClip()
            compose.onNodeWithContentDescription(files.first().name).performTouchInput {
                longClick()
            }
            compose.onNodeWithText("Copier l’image").performClick()
            compose.onAllNodesWithTag("attachment-actions").assertCountEquals(0)
            compose.waitUntil(10000) {
                clipboard.primaryClip?.description?.label == files.first().name
            }
            assertNotNull(clipboard.primaryClip?.getItemAt(0)?.uri)
            assertTrue("shot/stream?" in requested)

            compose.onNodeWithContentDescription("IMG_20261017_141502_HDR.jpg").performClick()
            compose.onNodeWithTag("attachment-viewer").assertExists()
            compose.onNodeWithText("JPG · 3,2 Mo · 2 / 6").assertExists()
            compose.waitUntil(10000) { "hdr/stream?" in requested }
            capture("02-viewer")
            // The filmstrip jumps between files (Robolectric cannot render PDF pages, so the
            // journey stays among the pictures).
            compose.onNode(hasClickLabel("Screenshot_20261017-142133_Chrome.png")).performClick()
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("PNG · 1,8 Mo · 1 / 6")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithContentDescription("Fermer").performClick()
            compose.waitUntil(5000) {
                compose.onAllNodesWithTag("attachment-viewer").fetchSemanticsNodes().isEmpty()
            }

            compose.onNodeWithText("export_clients_2026-10-17.csv").performTouchInput {
                longClick()
            }
            compose.onNodeWithTag("attachment-actions").assertExists()
            capture("03-actions")
            compose.onNodeWithText("Réutiliser dans un nouveau message").performClick()
            // The same file joins the draft without a new upload.
            compose.waitUntil(5000) {
                compose
                    .onAllNodesWithContentDescription("Retirer export_clients_2026-10-17.csv")
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").assertIsEnabled()
            capture("04-composer")

            // Reuse cannot change the attachment list while a send is in progress.
            val release = CompletableDeferred<Unit>()
            compose.runOnIdle { vm.perform { release.await() } }
            compose.waitUntil(10000) { vm.state.value.busy }
            try {
                compose.onNodeWithText("maquettes-sources.zip").performTouchInput { longClick() }
                compose.onNodeWithText("Réutiliser dans un nouveau message").performClick()
                compose.runOnIdle {
                    assertEquals(
                        listOf("export"),
                        vm.chatDrafts["chat"]!!.attachments.map { it.attachment.id },
                    )
                }
            } finally {
                release.complete(Unit)
            }
            compose.waitUntil(10000) { !vm.state.value.busy }

            // Refused cards survive disposing the conversation, just like its text and files.
            val oversized = File(app.cacheDir, "cairn-files/oversized.png")
            RandomAccessFile(oversized, "rw").use { it.setLength(MAX_ATTACHMENT_BYTES + 1) }
            val uri = FileProvider.getUriForFile(app, "${app.packageName}.files", oversized)
            compose.runOnIdle {
                clipboard.setPrimaryClip(ClipData.newUri(app.contentResolver, "oversized", uri))
            }
            compose.onNode(hasSetTextAction()).performSemanticsAction(SemanticsActions.PasteText)
            compose.waitUntil(10000) {
                compose
                    .onAllNodesWithText("Trop lourd", substring = true)
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").assertIsNotEnabled()
            compose.runOnIdle { show = false }
            compose.waitForIdle()
            assertEquals("oversized.png", vm.chatDrafts["chat"]!!.rejected.single().name)
            compose.runOnIdle { show = true }
            compose.onNodeWithText("Trop lourd", substring = true).assertExists()
            compose.onNodeWithTag("conversation-send").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Retirer oversized.png").performClick()
            // Restoring the screen also reloads its chat; sending waits for that read to finish.
            compose.waitUntil(10000) {
                compose
                    .onAllNodes(hasTestTag("conversation-send") and isEnabled())
                    .fetchSemanticsNodes()
                    .isNotEmpty()
            }
            compose.onNodeWithTag("conversation-send").assertIsEnabled()

            compose.onNodeWithContentDescription("Joindre").performClick()
            compose.onNodeWithTag("attach-sheet").assertExists()
            compose.onNodeWithText("Photos et vidéos").assertExists()
            compose.onNodeWithText("Fichiers").assertExists()
            capture("05-attach-sheet")
        }
    }

    private fun hasClickLabel(label: String) =
        SemanticsMatcher("click label $label") {
            it.config
                .getOrElseNullable(androidx.compose.ui.semantics.SemanticsActions.OnClick) {
                    null
                }
                ?.label == label
        }
}

private class AttachmentVault : SessionVault {
    private val values = mutableMapOf<String, String>()

    override fun read(origin: String) = values[origin]

    override fun write(origin: String, cookie: String?) {
        if (cookie == null) values.remove(origin) else values[origin] = cookie
    }
}
