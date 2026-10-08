package dev.leo.manager.data

import java.io.IOException
import java.nio.ByteBuffer
import java.util.Base64
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class DirectTransportTest {
    @Test
    fun `unidentified messages and other mutations fail visibly without relay replay`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            for ((method, path, payload) in
                listOf(
                    Triple("POST", "/chats/chat/messages", body("text" to "Bonjour")),
                    Triple("POST", "/chats", body("id" to "client-id")),
                    Triple("PATCH", "/chats/chat", body("title" to "Nouveau")),
                    Triple("DELETE", "/chats/chat", null),
                )) {
                api.transport.attach(
                    DirectChannel({ throw IOException("acknowledgement lost") }, {})
                )
                assertTrue(
                    runCatching { api.request(method, path, payload) }.exceptionOrNull()
                        is IOException
                )
                assertEquals(0, server.requestCount)
                assertEquals("relay", api.transport.route.value)
            }
        }
    }

    @Test
    fun `binary resources remain on relay while direct is attached`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("image"))
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            var direct = false
            api.transport.attach(DirectChannel({ direct = true }, {}))
            assertEquals("image", String(api.agentPortrait("agent", "revision")))
            assertFalse(direct)
            assertEquals(
                "/api/installations/test/api/agents/agent/avatar?v=revision",
                server.takeRequest().path,
            )
            api.closeStreams()
        }
    }

    @Test
    fun `canceling one request keeps the peer available for the next read`() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            val sent = java.util.concurrent.CountDownLatch(1)
            var disposed = false
            var respond = false
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet ->
                        val frame =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        if (frame["type"]!!.jsonPrimitive.content == "request") {
                            sent.countDown()
                            if (respond) {
                                val response = buildJsonObject {
                                    put("type", "response")
                                    put("id", frame["id"]!!)
                                    put("status", 200)
                                    put("headers", buildJsonArray {})
                                    put("body", "e30=")
                                }
                                    .toString()
                                    .toByteArray()
                                channel.receive(
                                    ByteBuffer.allocate(13 + response.size)
                                        .put(1)
                                        .putInt(99)
                                        .putInt(response.size)
                                        .putInt(0)
                                        .put(response)
                                        .array()
                                )
                            }
                        }
                    },
                    { disposed = true },
                )
            api.transport.attach(channel)
            val read =
                kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.Default).launch {
                    api.request("GET", "/chats")
                }
            assertTrue(sent.await(5, java.util.concurrent.TimeUnit.SECONDS))
            read.cancelAndJoin()
            respond = true
            assertEquals("{}", withTimeout(5000) { api.request("GET", "/chats") })
            assertFalse(disposed)
            assertEquals("direct", api.transport.route.value)
            assertEquals(0, server.requestCount)
            api.closeStreams()
        }
    }

    @Test
    fun `direct stream falls back at accepted cursor without missing or duplicate events`() =
        runBlocking {
            for (failure in listOf("closed", "corrupt", "credit")) {
                MockWebServer().use { server ->
                    fun sse(cursor: Long, events: List<RunEvent>) =
                        "event: batch\nid: $cursor\ndata: ${wireJson.encodeToString(LiveBatch(events, LiveState(run = Run("r1", status = "running")), false, false, history = "h1"))}\n\n"
                    val first = RunEvent(7, 7, "output", "direct")
                    val next = RunEvent(8, 8, "output", "relay")
                    server.enqueue(
                        MockResponse()
                            .setHeader("Content-Type", "text/event-stream")
                            .setBody(sse(8, listOf(first, next)))
                    )
                    server.start()
                    val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
                    lateinit var channel: DirectChannel
                    var id = ""
                    var credits = 0
                    fun deliver(frame: JsonObject) {
                        val bytes = frame.toString().toByteArray()
                        channel.receive(
                            ByteBuffer.allocate(13 + bytes.size)
                                .put(1)
                                .putInt(99)
                                .putInt(bytes.size)
                                .putInt(0)
                                .put(bytes)
                                .array()
                        )
                    }
                    channel =
                        DirectChannel(
                            { packet ->
                                val frame =
                                    wireJson
                                        .parseToJsonElement(
                                            String(packet.copyOfRange(13, packet.size))
                                        )
                                        .jsonObject
                                when (frame["type"]!!.jsonPrimitive.content) {
                                    "request" -> {
                                        id = frame["id"]!!.jsonPrimitive.content
                                        deliver(
                                            buildJsonObject {
                                                put("type", "stream_start")
                                                put("id", id)
                                                put("status", 200)
                                                put("body", "")
                                                put(
                                                    "headers",
                                                    buildJsonArray {
                                                        add(
                                                            buildJsonArray {
                                                                add("content-type")
                                                                add("text/event-stream")
                                                            }
                                                        )
                                                    },
                                                )
                                            }
                                        )
                                    }
                                    "stream_credit" ->
                                        if (++credits == 1)
                                            deliver(
                                                buildJsonObject {
                                                    put("type", "stream_chunk")
                                                    put("id", id)
                                                    put(
                                                        "body",
                                                        Base64.getEncoder()
                                                            .encodeToString(
                                                                sse(7, listOf(first)).toByteArray()
                                                            ),
                                                    )
                                                }
                                            )
                                }
                                if (
                                    frame["type"]!!.jsonPrimitive.content == "stream_credit" &&
                                        credits == 2
                                ) {
                                    if (failure == "credit") throw IOException("credit send failed")
                                    if (failure == "corrupt")
                                        deliver(
                                            buildJsonObject {
                                                put("type", "stream_chunk")
                                                put("id", id)
                                                put("body", "not-base64!")
                                            }
                                        )
                                }
                            },
                            {},
                        )
                    api.transport.attach(channel)
                    val final =
                        withTimeout(10000) {
                            api.live("/runs/r1/stream")
                                .onEach {
                                    if (it.cursor == 7L && failure == "closed") channel.close()
                                }
                                .first { it.cursor == 8L }
                        }
                    assertEquals(listOf(7L, 8L), final.events.map { it.id })
                    assertEquals(listOf("direct", "relay"), final.events.map { it.text })
                    assertEquals(
                        "/api/installations/test/api/runs/r1/stream?after=7&history=h1&window=1",
                        server.takeRequest().path,
                    )
                    assertEquals("relay", api.transport.route.value)
                    assertTrue(credits >= 1)
                    assertTrue(api.streamCalls.isEmpty())
                }
            }
        }

    @Test
    fun `direct message keeps its JSON body and content type`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            val message = body("id" to "3944c05c-ef02-459f-b47a-7762c79b2767", "text" to "Bonjour")
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet ->
                        val request =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        assertEquals(
                            message.toString(),
                            String(
                                Base64.getDecoder().decode(request["body"]!!.jsonPrimitive.content)
                            ),
                        )
                        assertTrue(
                            request["headers"]!!.jsonArray.any {
                                it.jsonArray[0].jsonPrimitive.content == "content-type" &&
                                    it.jsonArray[1].jsonPrimitive.content.substringBefore(';') ==
                                        "application/json"
                            }
                        )
                        val response = buildJsonObject {
                            put("type", "response")
                            put("id", request["id"]!!)
                            put("status", 200)
                            put("headers", buildJsonArray {})
                            put("body", "e30=")
                        }
                            .toString()
                            .toByteArray()
                        channel.receive(
                            ByteBuffer.allocate(13 + response.size)
                                .put(1)
                                .putInt(99)
                                .putInt(response.size)
                                .putInt(0)
                                .put(response)
                                .array()
                        )
                    },
                    {},
                )
            api.transport.attach(channel)
            assertEquals("{}", api.request("POST", "/chats/chat/messages", message))
            assertEquals("direct", api.transport.route.value)
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `lost message acknowledgement replays its client identifier and duplicate is success`() =
        runTest {
            MockWebServer().use { server ->
                server.enqueue(
                    MockResponse()
                        .setResponseCode(409)
                        .setBody("{\"error\":\"This message identifier has already been used.\"}")
                )
                server.start()
                val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
                var attempted = false
                api.transport.attach(
                    DirectChannel(
                        {
                            attempted = true
                            throw IOException("acknowledgement lost")
                        },
                        {},
                    )
                )
                val message =
                    body("id" to "3944c05c-ef02-459f-b47a-7762c79b2767", "text" to "Bonjour")
                assertEquals("{}", api.request("POST", "/chats/chat/messages", message))
                assertTrue(attempted)
                assertEquals(message.toString(), server.takeRequest().body.readUtf8())
                assertEquals(1, server.requestCount)
                assertEquals("relay", api.transport.route.value)
            }
        }

    @Test
    fun `large application responses use existing ordered 16 KiB fragments`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            val expected = "{\"text\":\"${"é".repeat(18000)}\"}"
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet ->
                        val request =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        val response = buildJsonObject {
                            put("type", "response")
                            put("id", request["id"]!!)
                            put("status", 200)
                            put("headers", buildJsonArray {})
                            put("body", Base64.getEncoder().encodeToString(expected.toByteArray()))
                        }
                            .toString()
                            .toByteArray()
                        var offset = 0
                        while (offset < response.size) {
                            val length = minOf(16371, response.size - offset)
                            channel.receive(
                                ByteBuffer.allocate(13 + length)
                                    .put(1)
                                    .putInt(73)
                                    .putInt(response.size)
                                    .putInt(offset)
                                    .put(response, offset, length)
                                    .array()
                            )
                            offset += length
                        }
                    },
                    {},
                )
            api.transport.attach(channel)
            assertEquals(expected, api.request("GET", "/chats"))
            assertEquals("direct", api.transport.route.value)
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `authorized channel carries a read and exposes the actual direct route`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet ->
                        val request =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        assertEquals("/api/chats?after=7", request["path"]!!.jsonPrimitive.content)
                        assertEquals("GET", request["method"]!!.jsonPrimitive.content)
                        val response = buildJsonObject {
                            put("type", "response")
                            put("id", request["id"]!!)
                            put("status", 200)
                            put(
                                "headers",
                                buildJsonArray {
                                    add(
                                        buildJsonArray {
                                            add("content-type")
                                            add("application/json")
                                        }
                                    )
                                },
                            )
                            put(
                                "body",
                                Base64.getEncoder()
                                    .encodeToString("{\"direct\":true}".toByteArray()),
                            )
                        }
                            .toString()
                            .toByteArray()
                        channel.receive(
                            ByteBuffer.allocate(13 + response.size)
                                .put(1)
                                .putInt(99)
                                .putInt(response.size)
                                .putInt(0)
                                .put(response)
                                .array()
                        )
                    },
                    {},
                )
            api.transport.attach(channel)
            assertEquals("{\"direct\":true}", api.request("GET", "/chats?after=7"))
            assertEquals("direct", api.transport.route.value)
            assertEquals(0, server.requestCount)
        }
    }

    @Test
    fun `first read uses relay and broken direct read recovers on relay`() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"route\":\"relay\"}"))
            server.enqueue(MockResponse().setBody("{\"route\":\"recovered\"}"))
            server.start()
            val api = LeoApi(server.url("/"), MemoryVault(), installationId = "test")
            assertEquals("{\"route\":\"relay\"}", api.request("GET", "/chats"))
            assertEquals("relay", api.transport.route.value)
            var attempted = false
            val channel =
                DirectChannel(
                    {
                        attempted = true
                        throw IOException("UDP lost")
                    },
                    {},
                )
            api.transport.attach(channel)
            assertEquals("{\"route\":\"recovered\"}", api.request("GET", "/chats"))
            assertEquals("relay", api.transport.route.value)
            assertTrue("The direct route must have carried the failed read", attempted)
            assertEquals(2, server.requestCount)
        }
    }
}
