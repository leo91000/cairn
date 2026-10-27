package build.cairn.app.data

import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test

class LiveTest {
    private fun message(id: Long, text: String, delta: Boolean = false, itemId: String = "answer") =
        RunEvent(id = id,
createdAt = id * 10,
type = "item.updated",
text = "",
payload = kotlinx.serialization.json.JsonObject(mapOf(
                "item" to
                    buildJsonObject {
                        put("id", itemId)
                        put("type", "agent_message")
                        put(if (delta) "delta" else "text", text)
                    }
            )),
runId = "r1")

    private fun frame(cursor: Long, batch: LiveBatch) =
        "event: batch\nid: $cursor\ndata: ${wireJson.encodeToString(batch)}\n\n"

    @Test
    fun `older pages survive an IOException reconnect while the session lock remains occupied`() =
        runBlocking {
            MockWebServer().use { server ->
                val history = "v1:r1:1"
                val state =
                    LiveState(
                        run =
                            Run(
                                id = "r1",
                                status = "running",
                                taskId = "",
                            )
                    )
                for (id in listOf(12L, 13L)) {
                    server.enqueue(
                        MockResponse()
                            .setHeader("Content-Type", "text/event-stream")
                            .setBody(
                                frame(
                                    id,
                                    LiveBatch(
                                        listOf(message(id, "Message $id", itemId = "m$id")),
                                        state,
                                        false,
                                        false,
                                        history,
                                        oldest = 12,
                                        hasOlder = true,
                                    ),
                                )
                            )
                    )
                }
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val session = LiveSession().also { it.followsLatest = false }
                val pages = OlderPages()
                var applied: LiveSnapshot? = null
                var reconnect: LiveSnapshot? = null
                val final =
                    withTimeout(10000) {
                        api.live("/runs/r1/stream", session)
                            .map { snapshot ->
                                // A prepend taking this mutex cannot run until collection stops.
                                assertFalse(session.mutex.tryLock())
                                if (applied == null && snapshot.history == history) {
                                    applied =
                                        pages.apply(
                                            HistoryPage(
                                                listOf(message(11, "Older", itemId = "m11")),
                                                history,
                                                11,
                                                false,
                                            ),
                                            snapshot,
                                            snapshot,
                                        )
                                }
                                pages.display(snapshot).also {
                                    if (it.status == "Reconnexion…" && it.cursor == 12L)
                                        reconnect = it
                                }
                            }
                            .first { it.cursor == 13L }
                    }
                assertEquals(listOf(11L, 12L), checkNotNull(applied).events.map { it.id })
                assertEquals(listOf(11L, 12L), checkNotNull(reconnect).events.map { it.id })
                assertEquals(listOf(11L, 12L, 13L), final.events.map { it.id })
                assertEquals(11L, final.oldest)
                assertFalse(final.hasOlder)
                assertFalse(session.mutex.isLocked)
                assertEquals("0", server.takeRequest().requestUrl!!.queryParameter("after"))
                assertEquals("12", server.takeRequest().requestUrl!!.queryParameter("after"))
                assertTrue(api.streamCalls.isEmpty())
            }
        }

    @Test
    fun `cached scoped subagents resume without duplicate workers`() = runBlocking {
        run {
            MockWebServer().use { server ->
                val state =
                    LiveState(
                        run =
                            Run(
                                id = "r1",
                                status = "running",
                                taskId = "",
                            )
                    )
                fun item(id: Long, body: JsonObjectBuilder.() -> Unit) =
                    RunEvent(id = id,
createdAt = id,
type = "item.completed",
text = "",
payload = kotlinx.serialization.json.JsonObject(mapOf("item" to buildJsonObject(body))),
runId = "r1")
                val spawn =
                    item(1) {
                        put("id", "worker")
                        put("type", "subagent")
                        put("name", "Reviewer")
                        put("status", "in_progress")
                    }
                val progress =
                    item(2) {
                        put("id", "read")
                        put("type", "command_execution")
                        put("parent_id", "worker")
                        put("command", "pwd")
                        put("exit_code", 0)
                    }
                val finished =
                    spawn.copy(
                        id = 3,
                        payload =
                            mapOf(
                                "item" to
                                    JsonObject(
                                        spawn.payload!!.getValue("item").jsonObject +
                                            ("status" to JsonPrimitive("completed"))
                                    )
                            ),
                    )
                val value = CachedHistory(1, "v1:r1:1", state, listOf(spawn))
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            val reset = request.requestUrl!!.queryParameter("after") == "0"
                            val events =
                                if (reset) listOf(spawn, progress, finished)
                                else listOf(progress, finished)
                            val batch = LiveBatch(events, state, reset, false, value.history)
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame(3, batch))
                        }
                    }
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val route = "/runs/r1/stream"
                val session = LiveSession()
                session.restore(value, route, api.streamGeneration.get())
                val final = withTimeout(10000) { api.live(route, session).first { it.synced } }

                assertEquals(
                    "1",
                    server.takeRequest().requestUrl!!.queryParameter("after"),
                )
                val worker =
                    build.cairn.app.ui.timelineEntries(final.events).single().agents.single()
                assertEquals(build.cairn.app.ui.SubagentState.DONE, worker.state)
                assertEquals(1, worker.steps.size)
                assertTrue(final.events.all { it.runId == "r1" })
            }
        }
    }

    @Test
    fun `revalidation retains the cached paging boundary and a reset replaces it`() = runBlocking {
        for (reset in listOf(false, true)) {
            MockWebServer().use { server ->
                val revision = "v1:r1:1"
                val state =
                    LiveState(
                        run =
                            Run(
                                id = "r1",
                                status = "running",
                                taskId = "",
                            )
                    )
                server.enqueue(
                    MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(
                            frame(
                                350,
                                LiveBatch(
                                    emptyList(),
                                    state,
                                    reset,
                                    false,
                                    revision,
                                    oldest = 251,
                                    hasOlder = true,
                                ),
                            )
                        )
                )
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val path = "/runs/r1/stream"
                val session = LiveSession()
                session.restore(
                    CachedHistory(
                        350,
                        revision,
                        state,
                        (1L..350L).map { message(it, "saved", itemId = "m$it") },
                        oldest = 1,
                        hasOlder = false,
                    ),
                    path,
                    api.streamGeneration.get(),
                )
                val final = withTimeout(5000) { api.live(path, session).first { it.synced } }
                assertEquals(if (reset) 251L else 1L, final.oldest)
                assertEquals(reset, final.hasOlder)
                assertEquals(if (reset) 0 else 350, final.events.size)
                assertEquals("350", server.takeRequest().requestUrl!!.queryParameter("after"))
            }
        }
    }

    @Test
    fun `a reader following the latest output resumes on the recent window`() = runBlocking {
        for (follow in listOf(true, false)) {
            MockWebServer().use { server ->
                val revision = "v1:r1:1"
                val state =
                    LiveState(
                        run =
                            Run(
                                id = "r1",
                                status = "succeeded",
                                taskId = "",
                            )
                    )
                // The installation answers `recent=1` with its recent window as a history reset.
                server.dispatcher =
                    object : Dispatcher() {
                        override fun dispatch(request: RecordedRequest): MockResponse {
                            val recent = request.requestUrl!!.queryParameter("recent") == "1"
                            val batch =
                                LiveBatch(
                                    listOf(message(900, "Récent", itemId = "latest")),
                                    state,
                                    recent,
                                    false,
                                    revision,
                                    oldest = if (recent) 900 else null,
                                    hasOlder = recent,
                                )
                            return MockResponse()
                                .setHeader("Content-Type", "text/event-stream")
                                .setBody(frame(900, batch))
                        }
                    }
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val path = "/chats/c1/stream"
                val session = LiveSession()
                session.restore(
                    CachedHistory(
                        10,
                        revision,
                        state,
                        (1L..10L).map { message(it, "saved", itemId = "m$it") },
                        position = ReadingPosition(index = 3, follow = follow),
                        oldest = 1,
                    ),
                    path,
                    api.streamGeneration.get(),
                )
                val final = withTimeout(5000) { api.live(path, session).first { it.synced } }

                val request = server.takeRequest().requestUrl!!
                assertEquals("10", request.queryParameter("after"))
                assertEquals(if (follow) "1" else null, request.queryParameter("recent"))
                if (follow) {
                    // Older messages stay reachable from the new window's boundary.
                    assertEquals(listOf(900L), final.events.map { it.id })
                    assertEquals(900L, final.oldest)
                    assertTrue(final.hasOlder)
                    assertEquals(1, final.epoch)
                    assertNull(final.position)
                } else {
                    // The replay keeps the restored rows and the reader's place among them.
                    assertEquals((1L..10L).toList() + 900L, final.events.map { it.id })
                    assertEquals(1L, final.oldest)
                    assertEquals(0, final.epoch)
                    assertEquals(3, final.position?.index)
                }
            }
        }
    }

    @Test
    fun `slow display receives the complete answer without blocking or dropping wire deltas`() =
        runBlocking {
            MockWebServer().use { server ->
                val body = buildString {
                    append(
                        frame(
                            1,
                            LiveBatch(
                                listOf(message(1, "Bonjour")),
                                LiveState(
                                    run =
                                        Run(
                                            id = "r1",
                                            status = "running",
                                            taskId = "",
                                        )
                                ),
                                false,
                                false,
                            ),
                        )
                    )
                    for (id in 2L..101L) append(
                        frame(id, LiveBatch(listOf(message(id, " é$id", true)), null, false, false))
                    )
                }
                server.enqueue(
                    MockResponse().setHeader("Content-Type", "text/event-stream").setBody(body)
                )
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val session = LiveSession()
                val final =
                    withTimeout(15000) {
                        api.live("/runs/r1/stream", session)
                            .onEach {
                                // Simulate a display stalled while the producer keeps decoding.
                                while (session.cursor < 101) delay(10)
                            }
                            .first { it.cursor == 101L }
                    }
                assertEquals(
                    "Bonjour" + (2..101).joinToString("") { " é$it" },
                    final.events.single().text,
                )
                assertEquals(101L, final.cursor)
                assertEquals(1L, final.events.single().displayId)
                assertTrue(api.streamCalls.isEmpty())
            }
        }

    @Test
    fun `backwards page keeps latest message snapshot and intervening tools`() {
        fun message(id: Long, content: String) =
            RunEvent(id = id,
createdAt = id,
type = "item.updated",
text = content,
payload = kotlinx.serialization.json.JsonObject(mapOf(
                    "item" to
                        buildJsonObject {
                            put("id", "m")
                            put("type", "agent_message")
                            put("text", content)
                        }
                )))
        val merged =
            mergeHistory(
                listOf(message(1, "old"), RunEvent(id = 2,
createdAt = 2,
type = "output",
text = "tool")),
                listOf(message(3, "complete")),
            )
        assertEquals(2, merged.size)
        assertEquals("complete", merged[0].text)
        assertEquals("tool", merged[1].text)
        assertEquals(merged, mergeHistory(listOf(message(1, "old")), merged))
    }

    @Test
    fun `restoring a folded snapshot retains the delta baseline without duplicating a message`() {
        val first = LiveAccumulator()
        val saved = first.append(listOf(message(1, "Bonjour"), message(2, " Cairn", true)))
        val restored = LiveAccumulator()
        restored.restore(saved, 2)
        val result = restored.append(listOf(message(2, "duplicate", true), message(3, " !", true)))
        assertEquals(1, result.size)
        assertEquals("Bonjour Cairn !", result.single().text)
        assertEquals(3L, restored.cursor)
    }

    @Test
    fun `deltas fold assistant rows and a new turn isolates reused item identifiers`() {
        val accumulator = LiveAccumulator()
        accumulator.append(listOf(message(1, "Bonjour"), message(2, " Cairn", true)))
        val rows =
            accumulator.append(
                listOf(
                    message(2, "duplicate", true),
                    RunEvent(id = 3,
createdAt = 30,
type = "turn.started",
text = ""),
                    message(4, "Nouvelle réponse"),
                )
            )
        assertEquals(3, rows.size)
        assertEquals(
            "Bonjour Cairn",
            (rows[0].payload!!["item"] as JsonObject)["text"]!!.jsonPrimitive.content,
        )
        assertEquals(10L, rows[0].createdAt)
        assertEquals(4L, accumulator.cursor)
    }

    @Test
    fun `a corrupt delta batch does not commit its preceding events`() {
        val accumulator = LiveAccumulator()
        accumulator.append(listOf(message(1, "baseline")))
        assertThrows(IllegalArgumentException::class.java) {
            accumulator.append(
                listOf(RunEvent(id = 2,
createdAt = 20,
type = "turn.started",
text = ""), message(3, "delta", true))
            )
        }
        assertEquals(1L, accumulator.cursor)
        assertEquals(1, accumulator.append(emptyList()).size)
    }

    @Test
    fun `a verified cached snapshot stays synced after the transport closes immediately`() =
        runBlocking {
            MockWebServer().use { server ->
                val revision = "v1:r1:1"
                server.enqueue(
                    MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(
                            frame(
                                1,
                                LiveBatch(
                                    emptyList(),
                                    LiveState(
                                        run =
                                            Run(
                                                id = "r1",
                                                status = "succeeded",
                                                taskId = "",
                                            )
                                    ),
                                    false,
                                    false,
                                    revision,
                                ),
                            )
                        )
                )
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val path = "/runs/r1/stream"
                val session = LiveSession()
                session.restore(
                    CachedHistory(
                        1,
                        revision,
                        LiveState(
                            run =
                                Run(
                                    id = "r1",
                                    status = "running",
                                    taskId = "",
                                )
                        ),
                        listOf(message(1, "saved")),
                    ),
                    path,
                    api.streamGeneration.get(),
                )
                assertFalse(session.snapshot.synced)
                val resumed =
                    withTimeout(10000) {
                        api.live(path, session).first { it.status == "Reconnexion…" }
                    }
                assertTrue(resumed.synced)
                assertFalse(resumed.catchingUp)
                assertEquals("succeeded", resumed.state?.run?.status)
                assertEquals(1, resumed.events.size)
            }
        }

    @Test
    fun `SSE reconnect sends accepted cursor and reset discards previous execution`() =
        runBlocking {
            MockWebServer().use { server ->
                val first =
                    LiveBatch(
                        listOf(message(1, "premier")),
                        LiveState(
                            run =
                                Run(
                                    id = "r1",
                                    status = "running",
                                    taskId = "",
                                )
                        ),
                        false,
                        false,
                    )
                val second =
                    LiveBatch(
                        listOf(message(1, "autre run")),
                        LiveState(
                            run =
                                Run(
                                    id = "r2",
                                    status = "running",
                                    taskId = "",
                                )
                        ),
                        true,
                        false,
                    )
                server.enqueue(
                    MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(frame(1, first))
                )
                server.enqueue(
                    MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(frame(1, second))
                )
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val snapshots =
                    withTimeout(10000) {
                        api.live("/chats/chat/stream")
                            .filter { it.events.isNotEmpty() }
                            .distinctUntilChangedBy { it.state?.run?.id }
                            .take(2)
                            .toList()
                    }
                assertEquals("/api/chats/chat/stream?after=0&window=1", server.takeRequest().path)
                assertEquals(
                    "/api/chats/chat/stream?after=1&window=1&recent=1",
                    server.takeRequest().path,
                )
                assertEquals("r2", snapshots.last().state?.run?.id)
                assertEquals(1, snapshots.last().events.size)
                assertTrue(api.streamCalls.isEmpty())
            }
        }

    @Test
    fun `retained session resumes compressed messages and isolates paths and generations`() =
        runBlocking {
            MockWebServer().use { server ->
                fun response(id: Long, event: RunEvent) =
                    MockResponse()
                        .setHeader("Content-Type", "text/event-stream")
                        .setBody(
                            frame(
                                id,
                                LiveBatch(
                                    listOf(event),
                                    LiveState(
                                        run =
                                            Run(
                                                id = "r1",
                                                status = "running",
                                                taskId = "",
                                            )
                                    ),
                                    false,
                                    false,
                                ),
                            )
                        )
                server.enqueue(response(1, message(1, "Bonjour")))
                server.enqueue(response(2, message(2, " Cairn", true)))
                server.enqueue(response(1, message(1, "Autre conversation")))
                server.enqueue(response(1, message(1, "Nouvelle session")))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault())
                val session = LiveSession()
                withTimeout(10000) {
                    api.live("/chats/one/stream", session).first {
                        it.events.lastOrNull()?.id == 1L
                    }
                    val resumed =
                        api.live("/chats/one/stream", session).first {
                            it.events.lastOrNull()?.id == 2L
                        }
                    assertEquals("Bonjour Cairn", resumed.events.single().text)
                    val other =
                        api.live("/chats/two/stream", session).first { it.events.isNotEmpty() }
                    assertEquals(
                        "Autre conversation",
                        other.events
                            .single()
                            .payload!!["item"]!!
                            .jsonObject["text"]!!
                            .jsonPrimitive
                            .content,
                    )
                    api.closeStreams()
                    val fresh =
                        api.live("/chats/two/stream", session).first { it.events.isNotEmpty() }
                    assertEquals(
                        "Nouvelle session",
                        fresh.events
                            .single()
                            .payload!!["item"]!!
                            .jsonObject["text"]!!
                            .jsonPrimitive
                            .content,
                    )
                }
                assertEquals("/api/chats/one/stream?after=0&window=1", server.takeRequest().path)
                assertEquals(
                    "/api/chats/one/stream?after=1&window=1&recent=1",
                    server.takeRequest().path,
                )
                assertEquals("/api/chats/two/stream?after=0&window=1", server.takeRequest().path)
                assertEquals("/api/chats/two/stream?after=0&window=1", server.takeRequest().path)
            }
        }

    @Test
    fun `revoked SSE session stops retrying and reports its HTTP status`() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse().setResponseCode(401).setBody("{\"error\":\"Session expired\"}")
            )
            server.start()
            val snapshots =
                withTimeout(5000) {
                    CairnApi(server.url("/"), MemoryVault()).live("/chats/stream").toList()
                }
            assertEquals(401, snapshots.single().httpStatus)
            assertEquals(1, server.requestCount)
        }
    }

    @Test
    fun `logout cancels an open SSE read immediately`() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.NO_RESPONSE))
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault())
            val collection = launch(Dispatchers.Default) { api.live("/chats/stream").collect() }
            assertNotNull(withContext(Dispatchers.IO) { server.takeRequest(5, TimeUnit.SECONDS) })
            api.closeStreams()
            withTimeout(3000) { collection.join() }
            assertTrue(api.streamCalls.isEmpty())
        }
    }

    @Test
    fun `artifact identity is origin scoped and only the latest version of each key is selected`() {
        val older = Deliverable("old", "run", key = "report", version = 1)
        val latest = older.copy(id = "new", version = 2)
        val anotherRun = older.copy(runId = "other")
        assertEquals(
            setOf(latest, anotherRun),
            latestArtifacts(listOf(latest, older, anotherRun)).toSet(),
        )
        val fromWire =
            wireJson.decodeFromString<Deliverable>(
                """{"id":"a","runId":"r","key":"k","url":"https://hostile.example/steal"}"""
            )
        assertEquals("/runs/r/artifacts/a?download=1", fromWire.path())
    }

    @Test
    fun `binary transfers include authentication and reject oversized downloads without a partial file`() =
        runBlocking {
            MockWebServer().use { server ->
                server.start()
                val vault =
                    MemoryVault().apply {
                        write(
                            server.url("/").toString(),
                            "cairn_session=fixture; Path=/; Max-Age=3600",
                        )
                    }
                val api = CairnApi(server.url("/"), vault).apply { csrf = "fixture-csrf" }
                val file = File.createTempFile("cairn-file", ".txt")
                try {
                    file.writeText("Contenu")
                    server.enqueue(
                        MockResponse().setBody("""{"id":"file","name":"notes.txt","size":7}""")
                    )
                    api.upload("/chats/chat/attachments/file?name=notes.txt", file)
                    val upload = server.takeRequest()
                    assertEquals("PUT", upload.method)
                    assertEquals("Contenu", upload.body.readUtf8())
                    assertEquals("cairn_session=fixture", upload.getHeader("Cookie"))
                    assertEquals("fixture-csrf", upload.getHeader("X-CSRF-Token"))
                    server.enqueue(MockResponse().setBody("123456789"))
                    try {
                        api.download("/runs/run/artifacts/file?download=1", file, 4)
                        fail("Oversized file accepted")
                    } catch (_: IllegalArgumentException) {}
                    assertFalse(file.exists())
                } finally {
                    file.delete()
                }
            }
        }
}
