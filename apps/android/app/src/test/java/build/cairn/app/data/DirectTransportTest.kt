package build.cairn.app.data

import java.io.IOException
import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.CountDownLatch
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okio.Buffer
import org.junit.Assert.*
import org.junit.Test

class DirectTransportTest {
    @Test
    fun `direct history negotiates gzip and decodes unicode JSON`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val expected = body("text" to "History 🦊 ".repeat(1000)).toString()
            val compressed =
                java.io
                    .ByteArrayOutputStream()
                    .also { output ->
                        java.util.zip.GZIPOutputStream(output).use {
                            it.write(expected.toByteArray())
                        }
                    }
                    .toByteArray()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
                        val request =
                            wireJson
                                .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                .jsonObject
                        assertTrue(
                            request["headers"]!!.jsonArray.any {
                                it.jsonArray[0].jsonPrimitive.content == "accept-encoding" &&
                                    it.jsonArray[1].jsonPrimitive.content == "gzip"
                            }
                        )
                        val frame = buildJsonObject {
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
                                    add(
                                        buildJsonArray {
                                            add("content-encoding")
                                            add("gzip")
                                        }
                                    )
                                    add(
                                        buildJsonArray {
                                            add("content-length")
                                            add(compressed.size.toString())
                                        }
                                    )
                                },
                            )
                            put(
                                "body",
                                if (request["method"]!!.jsonPrimitive.content == "HEAD") ""
                                else Base64.getEncoder().encodeToString(compressed),
                            )
                        }
                        channel.receive(singleFramePacket(frame.toString().toByteArray()))
                    },
                    {},
                )
            api.transport.attach(channel)
            val head =
                Request.Builder()
                    .url(server.url("/api/installations/test/api/chats"))
                    .head()
                    .build()
            channel.request(head, "/api/installations/test/api/", { false }).use {
                assertEquals("gzip", it.header("content-encoding"))
                assertEquals("", it.body.string())
            }
            assertEquals(expected, api.request("GET", "/runs/r/history?before=42&history=v1:r:1"))
            assertEquals("direct", api.transport.route.value)
            assertEquals(0, server.requestCount)
            api.closeStreams()
        }
    }

    @Test
    fun `stream paths request identity so JSON errors remain readable`() = runTest {
        for (path in listOf("/runs/r/stream?history=invalid", "/shared-artifacts/a/file")) {
            MockWebServer().use { server ->
                server.start()
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val request =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            val type = request["type"]!!.jsonPrimitive.content
                            if (type == "request") {
                                assertTrue(
                                    request["headers"]!!.jsonArray.any {
                                        it.jsonArray[0].jsonPrimitive.content ==
                                            "accept-encoding" &&
                                            it.jsonArray[1].jsonPrimitive.content == "identity"
                                    }
                                )
                                channel.receive(
                                    singleFramePacket(
                                        buildJsonObject {
                                            put("type", "stream_start")
                                            put("id", request["id"]!!)
                                            put("status", 400)
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
                                                    .encodeToString(
                                                        """{"error":"Invalid history version."}"""
                                                            .toByteArray()
                                                    ),
                                            )
                                        }
                                            .toString()
                                            .toByteArray()
                                    )
                                )
                            } else if (type == "stream_credit") {
                                channel.receive(
                                    singleFramePacket(
                                        buildJsonObject {
                                            put("type", "stream_end")
                                            put("id", request["id"]!!)
                                            put("failed", false)
                                        }
                                            .toString()
                                            .toByteArray()
                                    )
                                )
                            }
                        },
                        {},
                    )
                val request =
                    Request.Builder().url(server.url("/api/installations/test/api$path")).build()
                channel.request(request, "/api/installations/test/api/", { false }).use {
                    assertEquals(400, it.code)
                    assertEquals("""{"error":"Invalid history version."}""", it.body.string())
                }
                channel.close()
            }
        }
    }

    @Test
    fun `a protocol mismatch is actionable and never falls back to relay`() = runBlocking {
        val packets =
            listOf(
                ByteArray(13).apply { this[0] = 2 },
                singleFramePacket(
                    """{"type":"update_required","message":"Update required."}""".toByteArray()
                ),
            )
        for (packet in packets) {
            MockWebServer().use { server ->
                server.start()
                server.enqueue(MockResponse().setResponseCode(200))
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                val channel = DirectChannel({ _, _, _ -> }, {})
                api.transport.attach(channel)
                channel.receive(packet)
                withTimeout(5000) { api.transport.protocolError.first { it != null } }
                val failure = runCatching { api.request("GET", "/chats") }.exceptionOrNull()
                assertTrue(failure is ProtocolMismatch)
                assertFalse(isConnectionFailure(checkNotNull(failure)))
                assertTrue(failure.message.orEmpty().contains("Mise à jour requise"))
                val client = OkHttpClient.Builder().addInterceptor(api.transport).build()
                val request =
                    Request.Builder()
                        .url(server.url("/api/installations/test/api/chats/stream"))
                        .header("Accept", "text/event-stream")
                        .build()
                val streamFailure = runCatching {
                    api.transport.newCall(client, request).execute().use {}
                }
                    .exceptionOrNull()
                assertTrue(streamFailure is ProtocolMismatch)
                assertEquals(0, server.requestCount)
                api.closeStreams()
            }
        }
    }

    @Test
    fun `response parsing and base64 validation leave the callback thread`() {
        for (bytes in
            listOf(
                "{".toByteArray(),
                """{"type":"response","id":"r","status":200,"headers":[],"body":"not-base64!"}"""
                    .toByteArray(),
            )) {
            val disposed = java.util.concurrent.CountDownLatch(1)
            val decoderThread = java.util.concurrent.atomic.AtomicReference<Thread>()
            val callbackThread = Thread.currentThread()
            val channel =
                DirectChannel(
                    { _, _, _ -> },
                    {
                        decoderThread.set(Thread.currentThread())
                        disposed.countDown()
                    },
                )
            try {
                channel.receive(singleFramePacket(bytes))
                assertTrue(disposed.await(5, java.util.concurrent.TimeUnit.SECONDS))
                assertNotSame(
                    "Invalid response must be decoded by the worker",
                    callbackThread,
                    decoderThread.get(),
                )
            } finally {
                channel.close()
            }
        }
    }

    @Test
    fun `direct deadline leaves five seconds for the server timeout response`() {
        lateinit var channel: DirectChannel
        channel =
            DirectChannel(
                { packet, _, deadline ->
                    val remaining =
                        java.util.concurrent.TimeUnit.NANOSECONDS.toMillis(
                            deadline - System.nanoTime()
                        )
                    assertTrue(
                        "Server's 30 s response must precede the client deadline: $remaining",
                        remaining in 34000..35000,
                    )
                    val request =
                        wireJson
                            .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                            .jsonObject
                    channel.receive(
                        singleFramePacket(
                            buildJsonObject {
                                put("type", "response")
                                put("id", request["id"]!!)
                                put("status", 504)
                                put("headers", buildJsonArray {})
                                put("body", "e30=")
                            }
                                .toString()
                                .toByteArray()
                        )
                    )
                },
                {},
            )
        val request =
            Request.Builder().url("https://example.test/api/installations/test/api/chats").build()
        channel.request(request, "/api/installations/test/api/", { false }).use {
            assertEquals(504, it.code)
        }
        channel.close()
    }

    @Test
    fun `relay account and direct attempts keep their respective call deadlines`() = runBlocking {
        MockWebServer().use { server ->
            repeat(3) { server.enqueue(MockResponse().setBody("{}")) }
            server.start()
            val deadlines = java.util.concurrent.CopyOnWriteArrayList<Long>()
            val client =
                okhttp3.OkHttpClient.Builder()
                    .eventListener(
                        object : okhttp3.EventListener() {
                            override fun callStart(call: okhttp3.Call) {
                                deadlines.add(call.timeout().timeoutNanos())
                            }
                        }
                    )
                    .build()
            val api = CairnApi(server.url("/"), MemoryVault(), client, "test")
            try {
                api.request("GET", "/chats")
                api.transport.attach(
                    DirectChannel({ _, _, _ -> throw IOException("direct unavailable") }, {})
                )
                api.request("GET", "/account/session")
                api.request("GET", "/chats")
                assertEquals(
                    listOf(30L, 30L, 65L).map(java.util.concurrent.TimeUnit.SECONDS::toNanos),
                    deadlines,
                )
            } finally {
                api.closeStreams()
            }
        }
    }

    @Test
    fun `bounded write pressure recovers a read on relay without destroying the peer`() =
        runBlocking {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("[]"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                var disposed = false
                var pressured = true
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val frame =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            if (frame["type"]!!.jsonPrimitive.content == "request") {
                                if (pressured) throw DirectRequestTimeout()
                                channel.receive(
                                    singleFramePacket(
                                        buildJsonObject {
                                            put("type", "response")
                                            put("id", frame["id"]!!)
                                            put("status", 200)
                                            put("headers", buildJsonArray {})
                                            put("body", "e30=")
                                        }
                                            .toString()
                                            .toByteArray()
                                    )
                                )
                            }
                        },
                        { disposed = true },
                    )
                api.transport.attach(channel)
                try {
                    assertEquals("[]", api.request("GET", "/chats"))
                    assertFalse(disposed)
                    pressured = false
                    assertEquals("{}", api.request("GET", "/chats"))
                    assertEquals("direct", api.transport.route.value)
                    assertEquals(1, server.requestCount)
                } finally {
                    api.closeStreams()
                }
            }
        }

    @Test
    fun `fragmented message write pressure can return a relay rejection before direct recovers`() =
        runBlocking {
            data class RelayRequest(val method: String, val path: String, val body: JsonElement)

            val relayRequests = ConcurrentLinkedQueue<RelayRequest>()
            lateinit var api: CairnApi
            val client =
                OkHttpClient.Builder()
                    .addInterceptor { chain ->
                        // Use the real route owner with an in-process relay continuation.
                        // CairnApi still encodes the request and decodes the 413 response.
                        api.transport.intercept(
                            object : Interceptor.Chain by chain {
                                override fun proceed(request: Request): Response {
                                    val encoded = Buffer()
                                    requireNotNull(request.body).writeTo(encoded)
                                    relayRequests.add(
                                        RelayRequest(
                                            request.method,
                                            request.url.encodedPath,
                                            wireJson.parseToJsonElement(encoded.readUtf8()),
                                        )
                                    )
                                    return Response.Builder()
                                        .request(request)
                                        .protocol(Protocol.HTTP_1_1)
                                        .code(413)
                                        .message("Payload rejected")
                                        .body(
                                            """{"error":"Fixture relay rejection."}"""
                                                .toResponseBody("application/json".toMediaType())
                                        )
                                        .build()
                                }
                            }
                        )
                    }
                    .build()
            api = CairnApi("http://localhost/".toHttpUrl(), MemoryVault(), client, "test")
            var disposed = false
            var partialTransfer: Int? = null
            var abortedTransfer: Int? = null
            var acceptedFragments = 0
            var pressuredFragments = 0
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
                        val envelope = ByteBuffer.wrap(packet)
                        val transfer = envelope.getInt(1)
                        val total = envelope.getInt(5)
                        val offset = envelope.getInt(9)
                        if (total == 0) {
                            abortedTransfer = transfer
                        } else if (total > DirectChannel.MAX_PACKET) {
                            if (offset == 0) {
                                partialTransfer = transfer
                                acceptedFragments++
                            } else {
                                pressuredFragments++
                                throw DirectRequestTimeout()
                            }
                        } else {
                            val request =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            if (request["type"]!!.jsonPrimitive.content == "request")
                                channel.receive(
                                    singleFramePacket(
                                        buildJsonObject {
                                            put("type", "response")
                                            put("id", request["id"]!!)
                                            put("status", 200)
                                            put("headers", buildJsonArray {})
                                            put("body", "W10=")
                                        }
                                            .toString()
                                            .toByteArray()
                                    )
                                )
                        }
                    },
                    { disposed = true },
                )
            api.transport.attach(channel)
            try {
                assertEquals("[]", api.request("GET", "/chats"))
                assertEquals("direct", api.transport.route.value)
                assertEquals(0, relayRequests.size)
                val message =
                    body(
                        "id" to "pressured-message",
                        "text" to "x".repeat(DirectChannel.MAX_PACKET),
                    )
                val rejected =
                    try {
                        api.request("POST", "/chats/missing/messages", message)
                        throw AssertionError("Expected the relay rejection")
                    } catch (error: ApiException) {
                        error
                    }
                assertEquals(413, rejected.status)
                assertEquals("Fixture relay rejection.", rejected.message)
                assertEquals(1, acceptedFragments)
                assertEquals(1, pressuredFragments)
                assertNotNull(partialTransfer)
                assertEquals(partialTransfer, abortedTransfer)
                assertFalse(disposed)
                assertEquals("relay", api.transport.route.value)
                assertEquals("[]", api.request("GET", "/chats"))
                assertEquals("direct", api.transport.route.value)
                assertEquals(1, relayRequests.size)
                val replay = relayRequests.single()
                assertEquals("POST", replay.method)
                assertEquals("/api/installations/test/api/chats/missing/messages", replay.path)
                assertEquals(message, replay.body)
            } finally {
                api.closeStreams()
            }
        }

    @Test
    fun `slow direct read recovers on relay without closing other requests on the peer`() =
        runBlocking {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("[]"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                var disposed = false
                var reads = 0
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val frame =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            if (frame["type"]!!.jsonPrimitive.content == "request" && ++reads > 1) {
                                channel.receive(
                                    singleFramePacket(
                                        buildJsonObject {
                                            put("type", "response")
                                            put("id", frame["id"]!!)
                                            put("status", 200)
                                            put("headers", buildJsonArray {})
                                            put("body", "e30=")
                                        }
                                            .toString()
                                            .toByteArray()
                                    )
                                )
                            }
                        },
                        { disposed = true },
                    )
                api.transport.attach(channel)
                try {
                    assertEquals("[]", withTimeout(40000) { api.request("GET", "/chats") })
                    assertFalse(disposed)
                    assertEquals(1, server.requestCount)
                    assertEquals("{}", api.request("GET", "/chats"))
                    assertEquals("direct", api.transport.route.value)
                    assertEquals(1, server.requestCount)
                } finally {
                    api.closeStreams()
                }
            }
        }

    @Test
    fun `reassembly refusal sends a mutation on relay and preserves the healthy peer`() =
        runBlocking {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("{\"route\":\"relay\"}"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                var disposed = false
                val sent = java.io.ByteArrayOutputStream()
                var packets = 0
                var fragmented = false
                val title = "x".repeat(20000)
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val envelope = ByteBuffer.wrap(packet)
                            val total = envelope.getInt(5)
                            sent.write(packet, 13, packet.size - 13)
                            packets++
                            if (sent.size() == total) {
                                val request =
                                    wireJson
                                        .parseToJsonElement(sent.toString(Charsets.UTF_8))
                                        .jsonObject
                                sent.reset()
                                if (request["type"]!!.jsonPrimitive.content == "request") {
                                    val rejected =
                                        request["method"]!!.jsonPrimitive.content == "POST"
                                    if (rejected) fragmented = packets > 1
                                    channel.receive(
                                        singleFramePacket(
                                            buildJsonObject {
                                                put("type", "response")
                                                put("id", request["id"]!!)
                                                put("status", if (rejected) 503 else 200)
                                                put(
                                                    "headers",
                                                    buildJsonArray {
                                                        if (rejected)
                                                            add(
                                                                buildJsonArray {
                                                                    add("x-cairn-direct-rejection")
                                                                    add("reassembly-busy")
                                                                }
                                                            )
                                                    },
                                                )
                                                put("body", "e30=")
                                            }
                                                .toString()
                                                .toByteArray()
                                        )
                                    )
                                }
                                packets = 0
                            }
                        },
                        { disposed = true },
                    )
                api.transport.attach(channel)
                try {
                    assertEquals(
                        "{\"route\":\"relay\"}",
                        api.request("POST", "/chats", body("title" to title)),
                    )
                    val relay = server.takeRequest()
                    assertEquals("POST", relay.method)
                    assertEquals("/api/installations/test/api/chats", relay.path)
                    assertEquals(
                        title,
                        wireJson
                            .parseToJsonElement(relay.body.readUtf8())
                            .jsonObject["title"]!!
                            .jsonPrimitive
                            .content,
                    )
                    assertTrue("The rejected mutation must span multiple packets", fragmented)
                    assertEquals("{}", api.request("GET", "/chats"))
                    assertEquals(1, server.requestCount)
                    assertEquals("direct", api.transport.route.value)
                    assertFalse(disposed)
                } finally {
                    api.closeStreams()
                }
            }
        }

    @Test
    fun `application errors and stream responses never authorize mutation replay`() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            try {
                for ((type, status, markers) in
                    listOf(
                        Triple("response", 503, emptyList()),
                        Triple("response", 503, listOf("unknown")),
                        Triple("response", 500, listOf("reassembly-busy")),
                        Triple("response", 503, listOf("reassembly-busy", "reassembly-busy")),
                        Triple("stream_start", 503, listOf("reassembly-busy")),
                    )) {
                    var disposed = false
                    lateinit var channel: DirectChannel
                    channel =
                        DirectChannel(
                            { packet, _, _ ->
                                val request =
                                    wireJson
                                        .parseToJsonElement(
                                            String(packet.copyOfRange(13, packet.size))
                                        )
                                        .jsonObject
                                if (request["type"]!!.jsonPrimitive.content == "request") {
                                    channel.receive(
                                        singleFramePacket(
                                            buildJsonObject {
                                                put("type", type)
                                                put("id", request["id"]!!)
                                                put("status", status)
                                                put(
                                                    "headers",
                                                    buildJsonArray {
                                                        markers.forEach { marker ->
                                                            add(
                                                                buildJsonArray {
                                                                    add("x-cairn-direct-rejection")
                                                                    add(marker)
                                                                }
                                                            )
                                                        }
                                                    },
                                                )
                                                put("body", "e30=")
                                            }
                                                .toString()
                                                .toByteArray()
                                        )
                                    )
                                    if (type == "stream_start")
                                        channel.receive(
                                            singleFramePacket(
                                                buildJsonObject {
                                                    put("type", "stream_end")
                                                    put("id", request["id"]!!)
                                                    put("failed", false)
                                                }
                                                    .toString()
                                                    .toByteArray()
                                            )
                                        )
                                }
                            },
                            { disposed = true },
                        )
                    api.transport.attach(channel)
                    val error = runCatching {
                        api.request("POST", "/chats", body("title" to "New chat"))
                    }
                        .exceptionOrNull()
                    assertTrue("$type $status $markers", error is ApiException)
                    assertEquals(status, (error as ApiException).status)
                    assertEquals(0, server.requestCount)
                    assertFalse(disposed)
                }
            } finally {
                api.closeStreams()
            }
        }
    }

    @Test
    fun `local saturation sends a mutation on relay and preserves the healthy peer`() =
        runBlocking {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("{}"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                val accepted = java.util.concurrent.CountDownLatch(32)
                val canceled = java.util.concurrent.atomic.AtomicBoolean()
                val respond = java.util.concurrent.atomic.AtomicBoolean()
                var disposed = false
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val frame =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            if (frame["type"]!!.jsonPrimitive.content == "request") {
                                if (respond.get()) {
                                    channel.receive(
                                        singleFramePacket(
                                            buildJsonObject {
                                                put("type", "response")
                                                put("id", frame["id"]!!)
                                                put("status", 200)
                                                put("headers", buildJsonArray {})
                                                put("body", "e30=")
                                            }
                                                .toString()
                                                .toByteArray()
                                        )
                                    )
                                } else accepted.countDown()
                            }
                        },
                        { disposed = true },
                    )
                api.transport.attach(channel)
                val prefix = "/api/installations/test/api/"
                val request = Request.Builder().url(server.url(prefix + "chats")).get().build()
                val workers =
                    List(32) {
                        kotlin.concurrent.thread(isDaemon = true) {
                            runCatching { channel.request(request, prefix, canceled::get).close() }
                        }
                    }
                try {
                    assertTrue(accepted.await(5, java.util.concurrent.TimeUnit.SECONDS))
                    assertEquals("{}", api.request("POST", "/chats", body("title" to "New chat")))
                    assertEquals(1, server.requestCount)
                    assertFalse(disposed)
                    canceled.set(true)
                    workers.forEach { it.join(5000) }
                    assertTrue(workers.none { it.isAlive })
                    respond.set(true)
                    assertEquals("{}", api.request("GET", "/chats"))
                    assertEquals("direct", api.transport.route.value)
                } finally {
                    canceled.set(true)
                    workers.forEach { it.join(5000) }
                    api.closeStreams()
                }
            }
        }

    @Test
    fun `mutation on an already closed channel uses relay without attempting direct`() =
        runBlocking {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("{}"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                val channel =
                    DirectChannel({ _, _, _ -> fail("A closed channel must not send") }, {})
                channel.close()
                api.transport.attach(channel)

                assertEquals("{}", api.request("POST", "/chats", body("title" to "New chat")))
                assertEquals(1, server.requestCount)
                assertEquals("relay", api.transport.route.value)
            }
        }

    @Test
    fun `invalid finite response closes direct and safe read recovers on relay`() = runBlocking {
        for (invalid in listOf("body", "status", "headers")) {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("{\"route\":\"relay\"}"))
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                val disposed = CompletableDeferred<Unit>()
                val disposalStarted = CompletableDeferred<Unit>()
                val allowDisposal = CountDownLatch(1)
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            val request =
                                wireJson
                                    .parseToJsonElement(String(packet.copyOfRange(13, packet.size)))
                                    .jsonObject
                            if (request["type"]!!.jsonPrimitive.content == "request") {
                                val response = buildJsonObject {
                                    put("type", "response")
                                    put("id", request["id"]!!)
                                    put(
                                        "status",
                                        if (invalid == "status") JsonPrimitive("invalid")
                                        else JsonPrimitive(200),
                                    )
                                    put(
                                        "headers",
                                        if (invalid == "headers") JsonPrimitive("invalid")
                                        else buildJsonArray {},
                                    )
                                    put("body", if (invalid == "body") "not-base64!" else "e30=")
                                }
                                channel.receive(
                                    singleFramePacket(response.toString().toByteArray())
                                )
                            }
                        },
                        {
                            disposalStarted.complete(Unit)
                            allowDisposal.await()
                            disposed.complete(Unit)
                        },
                    )
                api.transport.attach(channel)
                try {
                    assertEquals(
                        "{\"route\":\"relay\"}",
                        withTimeout(5000) { api.request("GET", "/chats") },
                    )
                    withTimeout(5000) { disposalStarted.await() }
                    // A safe read may recover while native disposal is still running.
                    assertFalse(disposed.isCompleted)
                    assertEquals("relay", api.transport.route.value)
                    assertEquals(1, server.requestCount)
                    server.enqueue(MockResponse().setBody("[]"))
                    assertEquals("[]", withTimeout(5000) { api.request("GET", "/chats") })
                    assertEquals(2, server.requestCount)
                } finally {
                    allowDisposal.countDown()
                    api.closeStreams()
                }
                withTimeout(5000) { disposed.await() }
            }
        }
    }

    @Test
    fun `oversized or excessive incomplete transfers close direct without blocking relay`() =
        runBlocking {
            for (invalid in listOf("frame", "packet", "assemblies")) {
                MockWebServer().use { server ->
                    server.enqueue(MockResponse().setBody("[]"))
                    server.start()
                    val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                    var disposed = false
                    lateinit var channel: DirectChannel
                    channel =
                        DirectChannel(
                            { _, _, _ ->
                                // Independent protocol literals: frame cap 10,732,204; packet
                                // 16,384;
                                // no more than 32 simultaneous incomplete transfers.
                                when (invalid) {
                                    "frame" ->
                                        channel.receive(
                                            ByteBuffer.allocate(14)
                                                .put(1)
                                                .putInt(99)
                                                .putInt(10732205)
                                                .putInt(0)
                                                .put(123)
                                                .array()
                                        )
                                    "packet" ->
                                        channel.receive(
                                            ByteBuffer.allocate(16385)
                                                .put(1)
                                                .putInt(99)
                                                .putInt(16372)
                                                .putInt(0)
                                                .array()
                                        )
                                    "assemblies" ->
                                        for (id in 1..33) channel.receive(
                                            ByteBuffer.allocate(14)
                                                .put(1)
                                                .putInt(id)
                                                .putInt(2)
                                                .putInt(0)
                                                .put(123)
                                                .array()
                                        )
                                }
                            },
                            { disposed = true },
                        )
                    api.transport.attach(channel)
                    assertEquals("[]", withTimeout(5000) { api.request("GET", "/chats") })
                    assertTrue(disposed)
                    assertEquals("relay", api.transport.route.value)
                    assertEquals(1, server.requestCount)
                }
            }
        }

    @Test
    fun `canceling a fragmented send aborts only its transfer and peer remains usable`() =
        runBlocking {
            MockWebServer().use { server ->
                server.start()
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                val packets = mutableListOf<ByteArray>()
                var canceled = false
                var disposed = false
                var respond = false
                lateinit var channel: DirectChannel
                channel =
                    DirectChannel(
                        { packet, _, _ ->
                            packets.add(packet)
                            if (!respond && packet.size == 16384) canceled = true
                            if (respond) {
                                val frame =
                                    wireJson
                                        .parseToJsonElement(
                                            String(packet.copyOfRange(13, packet.size))
                                        )
                                        .jsonObject
                                if (frame["type"]!!.jsonPrimitive.content == "request")
                                    channel.receive(
                                        singleFramePacket(
                                            buildJsonObject {
                                                put("type", "response")
                                                put("id", frame["id"]!!)
                                                put("status", 200)
                                                put("headers", buildJsonArray {})
                                                put("body", "e30=")
                                            }
                                                .toString()
                                                .toByteArray()
                                        )
                                    )
                            }
                        },
                        { disposed = true },
                    )
                val prefix = "/api/installations/test/api/"
                val request =
                    Request.Builder()
                        .url(server.url(prefix + "chats"))
                        .post("x".repeat(25000).toRequestBody())
                        .build()
                val error = runCatching {
                    channel.request(request, prefix) { canceled }
                }
                    .exceptionOrNull()
                assertTrue(error is IOException && error !is DirectLost)
                assertEquals(16384, packets.first().size)
                assertTrue(ByteBuffer.wrap(packets.first(), 5, 4).int > 16384)
                val abort = ByteBuffer.wrap(packets[1])
                assertEquals(13, packets[1].size)
                assertEquals(1.toByte(), abort.get())
                assertEquals(ByteBuffer.wrap(packets.first(), 1, 4).int, abort.int)
                assertEquals(0, abort.int)
                assertEquals(0, abort.int)
                assertFalse(disposed)
                respond = true
                api.transport.attach(channel)
                assertEquals("{}", api.request("GET", "/chats"))
                assertEquals("direct", api.transport.route.value)
                assertEquals(0, server.requestCount)
                api.closeStreams()
            }
        }

    @Test
    fun `unidentified messages and other mutations fail visibly without relay replay`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            for ((method, path, payload) in
                listOf(
                    Triple("POST", "/chats/chat/messages", body("text" to "Bonjour")),
                    Triple("POST", "/chats", body("id" to "client-id")),
                    Triple("PATCH", "/chats/chat", body("title" to "Nouveau")),
                    Triple("DELETE", "/chats/chat", null),
                )) {
                api.transport.attach(
                    DirectChannel({ _, _, _ -> throw IOException("acknowledgement lost") }, {})
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
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            var direct = false
            api.transport.attach(DirectChannel({ _, _, _ -> direct = true }, {}))
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
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            val sent = java.util.concurrent.CountDownLatch(1)
            var disposed = false
            var respond = false
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
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
                                channel.receive(singleFramePacket(response))
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
            for (failure in listOf("closed", "corrupt", "credit", "end", "timeout")) {
                MockWebServer().use { server ->
                    fun sse(cursor: Long, events: List<RunEvent>) =
                        "event: batch\nid: $cursor\ndata: ${wireJson.encodeToString(LiveBatch(events, LiveState(run = Run(id = "r1",
status = "running",
taskId = "",)), false, false, history = "h1",))}\n\n"
                    val first =
                        RunEvent(
                            id = 7,
                            createdAt = 7,
                            type = "turn.waiting",
                            text = "direct",
                            payload =
                                kotlinx.serialization.json.JsonObject(
                                    mapOf(
                                        "tasks" to
                                            buildJsonArray {
                                                addJsonObject {
                                                    put("description", "Vérifier la CI")
                                                }
                                            }
                                    )
                                ),
                        )
                    val next =
                        RunEvent(
                            id = 8,
                            createdAt = 8,
                            type = "output",
                            text = "relay",
                        )
                    server.enqueue(
                        MockResponse()
                            .setHeader("Content-Type", "text/event-stream")
                            .setBody(sse(8, listOf(first, next)))
                    )
                    server.start()
                    val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                    lateinit var channel: DirectChannel
                    var id = ""
                    var credits = 0
                    var disposed = false
                    fun deliver(frame: JsonObject) {
                        val bytes = frame.toString().toByteArray()
                        channel.receive(singleFramePacket(bytes))
                    }
                    channel =
                        DirectChannel(
                            { packet, _, _ ->
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
                                    if (failure == "timeout") throw DirectRequestTimeout()
                                    if (failure == "credit") throw IOException("credit send failed")
                                    if (failure == "end")
                                        deliver(
                                            buildJsonObject {
                                                put("type", "stream_end")
                                                put("id", id)
                                                put("failed", "invalid")
                                            }
                                        )
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
                            { disposed = true },
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
                        build.cairn.app.ui.BackgroundWait(listOf("Vérifier la CI"), 7),
                        build.cairn.app.ui.backgroundWait(final.events),
                    )
                    assertEquals(
                        "/api/installations/test/api/runs/r1/stream?after=7&history=h1&window=1&recent=1",
                        server.takeRequest().path,
                    )
                    assertEquals("relay", api.transport.route.value)
                    assertTrue(credits >= 1)
                    if (failure == "timeout") assertFalse(disposed)
                    assertTrue(api.streamCalls.isEmpty())
                }
            }
        }

    @Test
    fun `direct message keeps its JSON body and content type`() = runTest {
        MockWebServer().use { server ->
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            val message = body("id" to "3944c05c-ef02-459f-b47a-7762c79b2767", "text" to "Bonjour")
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
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
                        channel.receive(singleFramePacket(response))
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
                val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
                var attempted = false
                api.transport.attach(
                    DirectChannel(
                        { _, _, _ ->
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
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            val expected = "{\"text\":\"${"é".repeat(3_900_000)}\"}"
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
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
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            lateinit var channel: DirectChannel
            channel =
                DirectChannel(
                    { packet, _, _ ->
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
                        channel.receive(singleFramePacket(response))
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
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "test")
            assertEquals("{\"route\":\"relay\"}", api.request("GET", "/chats"))
            assertEquals("relay", api.transport.route.value)
            var attempted = false
            val channel =
                DirectChannel(
                    { _, _, _ ->
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

    private fun singleFramePacket(frame: ByteArray): ByteArray =
        ByteBuffer.allocate(13 + frame.size)
            .put(1)
            .putInt(99)
            .putInt(frame.size)
            .putInt(0)
            .put(frame)
            .array()
}
