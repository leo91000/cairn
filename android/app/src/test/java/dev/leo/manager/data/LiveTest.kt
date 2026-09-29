package dev.leo.manager.data

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
        RunEvent(
            id,
            id * 10,
            "item.updated",
            "",
            mapOf(
                "item" to
                    buildJsonObject {
                        put("id", itemId)
                        put("type", "agent_message")
                        put(if (delta) "delta" else "text", text)
                    }
            ),
        )

    private fun frame(cursor: Long, batch: LiveBatch) =
        "event: batch\nid: $cursor\ndata: ${wireJson.encodeToString(batch)}\n\n"

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
                                LiveState(run = Run("r1", status = "running")),
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
                val api = LeoApi(server.url("/"), MemoryVault())
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
            RunEvent(
                id,
                id,
                "item.updated",
                content,
                mapOf(
                    "item" to
                        buildJsonObject {
                            put("id", "m")
                            put("type", "agent_message")
                            put("text", content)
                        }
                ),
            )
        val merged =
            mergeHistory(
                listOf(message(1, "old"), RunEvent(2, 2, "output", "tool")),
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
        val saved = first.append(listOf(message(1, "Bonjour"), message(2, " Leo", true)))
        val restored = LiveAccumulator()
        restored.restore(saved, 2)
        val result = restored.append(listOf(message(2, "duplicate", true), message(3, " !", true)))
        assertEquals(1, result.size)
        assertEquals("Bonjour Leo !", result.single().text)
        assertEquals(3L, restored.cursor)
    }

    @Test
    fun `deltas fold assistant rows and a new turn isolates reused item identifiers`() {
        val accumulator = LiveAccumulator()
        accumulator.append(listOf(message(1, "Bonjour"), message(2, " Leo", true)))
        val rows =
            accumulator.append(
                listOf(
                    message(2, "duplicate", true),
                    RunEvent(3, 30, "turn.started", ""),
                    message(4, "Nouvelle réponse"),
                )
            )
        assertEquals(3, rows.size)
        assertEquals(
            "Bonjour Leo",
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
                listOf(RunEvent(2, 20, "turn.started", ""), message(3, "delta", true))
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
                                    LiveState(run = Run("r1", status = "succeeded")),
                                    false,
                                    false,
                                    revision,
                                ),
                            )
                        )
                )
                server.start()
                val api = LeoApi(server.url("/"), MemoryVault())
                val path = "/runs/r1/stream"
                val session = LiveSession()
                session.restore(
                    CachedHistory(
                        1,
                        revision,
                        LiveState(run = Run("r1", status = "running")),
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
                        LiveState(run = Run(id = "r1", status = "running")),
                        false,
                        false,
                    )
                val second =
                    LiveBatch(
                        listOf(message(1, "autre run")),
                        LiveState(run = Run(id = "r2", status = "running")),
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
                val api = LeoApi(server.url("/"), MemoryVault())
                val snapshots =
                    withTimeout(10000) {
                        api.live("/chats/chat/stream")
                            .filter { it.events.isNotEmpty() }
                            .distinctUntilChangedBy { it.state?.run?.id }
                            .take(2)
                            .toList()
                    }
                assertEquals("/api/chats/chat/stream?after=0&window=1", server.takeRequest().path)
                assertEquals("/api/chats/chat/stream?after=1&window=1", server.takeRequest().path)
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
                                    LiveState(run = Run("r1", status = "running")),
                                    false,
                                    false,
                                ),
                            )
                        )
                server.enqueue(response(1, message(1, "Bonjour")))
                server.enqueue(response(2, message(2, " Leo", true)))
                server.enqueue(response(1, message(1, "Autre conversation")))
                server.enqueue(response(1, message(1, "Nouvelle session")))
                server.start()
                val api = LeoApi(server.url("/"), MemoryVault())
                val session = LiveSession()
                withTimeout(10000) {
                    api.live("/chats/one/stream", session).first {
                        it.events.lastOrNull()?.id == 1L
                    }
                    val resumed =
                        api.live("/chats/one/stream", session).first {
                            it.events.lastOrNull()?.id == 2L
                        }
                    assertEquals("Bonjour Leo", resumed.events.single().text)
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
                assertEquals("/api/chats/one/stream?after=1&window=1", server.takeRequest().path)
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
                    LeoApi(server.url("/"), MemoryVault()).live("/chats/stream").toList()
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
            val api = LeoApi(server.url("/"), MemoryVault())
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
                            "leo_session=fixture; Path=/; Max-Age=3600",
                        )
                    }
                val api = LeoApi(server.url("/"), vault).apply { csrf = "fixture-csrf" }
                val file = File.createTempFile("leo-file", ".txt")
                try {
                    file.writeText("Contenu")
                    server.enqueue(
                        MockResponse().setBody("""{"id":"file","name":"notes.txt","size":7}""")
                    )
                    api.upload("/chats/chat/attachments/file?name=notes.txt", file)
                    val upload = server.takeRequest()
                    assertEquals("PUT", upload.method)
                    assertEquals("Contenu", upload.body.readUtf8())
                    assertEquals("leo_session=fixture", upload.getHeader("Cookie"))
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
