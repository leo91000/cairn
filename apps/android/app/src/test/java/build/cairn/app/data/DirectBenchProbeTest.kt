package build.cairn.app.data

import java.nio.ByteBuffer
import java.util.Base64
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test

class DirectBenchProbeTest {
    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun `slow direct setup after lifecycle reads stays within the account read budget`() = runTest {
        val reads = AtomicInteger()
        MockWebServer().use { server ->
            server.dispatcher =
                object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        assertEquals("/api/installations/test/api/chats", request.path)
                        // The real installation shares 300 reads/minute across direct and relay.
                        return if (reads.incrementAndGet() <= 300) MockResponse().setBody("[]")
                        else
                            MockResponse()
                                .setResponseCode(429)
                                .setBody("{\"error\":\"Too many requests. Try again later.\"}")
                    }
                }
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
                        val response = buildJsonObject {
                            put("type", "response")
                            put("id", request["id"]!!)
                            put("status", if (reads.incrementAndGet() <= 300) 200 else 429)
                            put("headers", buildJsonArray {})
                            put("body", Base64.getEncoder().encodeToString("[]".toByteArray()))
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
            try {
                // Preserve the device bench's 8 x 8 in-flight lifecycle workload.
                List(8) {
                        async {
                            repeat(8) { assertEquals("[]", api.request("GET", "/chats")) }
                        }
                    }
                    .awaitAll()
                awaitDirectBenchRead(api) {
                    // A valid setup can use most of the existing 60-second deadline.
                    if (testScheduler.currentTime >= 50_000) api.transport.attach(channel)
                    assertEquals("[]", api.request("GET", "/chats"))
                }
                assertEquals("direct", api.transport.route.value)
                assertEquals(50_000L, testScheduler.currentTime)
                assertTrue(
                    "The observed response must use the real direct channel",
                    server.requestCount < reads.get(),
                )
                assertTrue(reads.get() <= 300)
            } finally {
                api.closeStreams()
            }
        }
    }
}
