package dev.leo.manager.data

import java.io.IOException
import java.nio.ByteBuffer
import java.util.Base64
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class DirectTransportTest {
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
