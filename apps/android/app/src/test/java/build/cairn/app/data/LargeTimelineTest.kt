package build.cairn.app.data

import build.cairn.contracts.LargeTimelineReference
import build.cairn.contracts.ReadLargeTimelineInput
import build.cairn.contracts.RunContentOutput
import build.cairn.contracts.TimelineContent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.encodeToString
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class LargeTimelineTest {
    @Test
    fun `older stream baselines do not corrupt a newer fetched card`() {
        fun card(revision: Long, text: String): TimelineCard =
            wireJson.decodeFromString(
                """{"id":"message:1:a","runId":"run","position":1,"revision":$revision,"source":$revision,"turn":1,"attempt":0,"createdAt":1,"updatedAt":$revision,"content":{"kind":"agentMessage","itemId":"a","text":"$text","state":"running"}}"""
            )
        val accumulator = LiveAccumulator()
        accumulator.append(
            emptyList(),
            listOf(build.cairn.contracts.TimelineChange.A(card(5, "latest"))),
        )
        accumulator.append(
            emptyList(),
            listOf(build.cairn.contracts.TimelineChange.A(card(3, "old"))),
        )
        accumulator.append(
            emptyList(),
            listOf(build.cairn.contracts.TimelineChange.B("message:1:a", 4, 4, " suffix", 4)),
        )
        val shown = accumulator.entries.single()
        assertEquals(5L, shown.revision)
        assertEquals("latest", (shown.content as TimelineContent.B).text)
    }

    @Test
    fun `loads utf8 chunks before returning the card`() = runTest {
        val json =
            """{"id":"message:1:é","runId":"run","position":1,"revision":2,"source":2,"turn":1,"attempt":0,"createdAt":1,"updatedAt":2,"content":{"kind":"agentMessage","itemId":"é","text":"🦊 résultat","state":"done"}}"""
        val split = json.indexOf("🦊")
        val first = json.substring(0, split)
        val second = json.substring(split)
        val next = first.toByteArray(Charsets.UTF_8).size.toLong()
        val total = json.toByteArray(Charsets.UTF_8).size.toLong()
        val digest = "a".repeat(64)
        MockWebServer().use { server ->
            fun response(offset: Long, data: String, continuation: Long?) =
                wireJson.encodeToString(
                    RunContentOutput(
                        data,
                        "utf-8",
                        "message:1:é",
                        null,
                        "json",
                        continuation,
                        offset,
                        "run",
                        digest,
                        total,
                    )
                )
            server.enqueue(MockResponse().setBody(response(0, first, next)))
            server.enqueue(MockResponse().setBody(response(next, second, null)))
            val api = CairnApi(server.url("/"), MemoryVault())
            val shell =
                wireJson.decodeFromString<TimelineCard>(
                    json.replace("🦊 résultat", "").replace("\"itemId\":\"é\"", "\"itemId\":\"\"")
                )
            val preview = LargeTimelineReference(shell)
            val card = api.readLargeTimeline("/runs/run/stream", listOf(preview)).single()
            assertEquals("🦊 résultat", (card.content as TimelineContent.B).text)
            val firstRequest =
                wireJson.decodeFromString<ReadLargeTimelineInput>(
                    server.takeRequest().body.readUtf8()
                )
            assertEquals(0L, firstRequest.offset)
            val secondRequest =
                wireJson.decodeFromString<ReadLargeTimelineInput>(
                    server.takeRequest().body.readUtf8()
                )
            assertEquals(next, secondRequest.offset)
            assertEquals(digest, secondRequest.sha256)
        }
    }

    @Test
    fun `proposal chunks preserve parent and completion identity before cursor application`() =
        runTest {
            val event =
                wireJson.decodeFromString<RunEvent>(
                    """{"id":8,"runId":"run","createdAt":1,"type":"item.completed","text":"🦊 approach","payload":{"item":{"id":"p","type":"proposed_plan","text":"🦊 approach"},"parent_tool_use_id":"child","request_id":"request"}}"""
                )
            val json = wireJson.encodeToString(event)
            val split = json.indexOf("🦊")
            val first = json.substring(0, split)
            val second = json.substring(split)
            val next = first.toByteArray(Charsets.UTF_8).size.toLong()
            val total = json.toByteArray(Charsets.UTF_8).size.toLong()
            MockWebServer().use { server ->
                fun response(offset: Long, data: String, continuation: Long?) =
                    wireJson.encodeToString(
                        RunContentOutput(
                            data = data,
                            encoding = "utf-8",
                            eventId = 8,
                            format = "json",
                            nextOffset = continuation,
                            offset = offset,
                            runId = "run",
                            sha256 = "a".repeat(64),
                            totalBytes = total,
                        )
                    )
                server.enqueue(MockResponse().setBody(response(0, first, next)))
                server.enqueue(MockResponse().setBody(response(next, second, null)))
                val api = CairnApi(server.url("/"), MemoryVault())
                val preview =
                    event.copy(
                        text = "",
                        payload =
                            kotlinx.serialization.json.buildJsonObject {
                                put(
                                    "_cairnEventContent",
                                    kotlinx.serialization.json.JsonPrimitive(true),
                                )
                            },
                    )
                assertEquals(
                    listOf(event),
                    api.readLargeEvents("/runs/run/stream", listOf(preview)),
                )
                assertEquals("/runs/run/event-content", server.takeRequest().path)
                val request =
                    wireJson.parseToJsonElement(server.takeRequest().body.readUtf8()).toString()
                org.junit.Assert.assertTrue(request.contains("\"sha256\":\"${"a".repeat(64)}\""))
                // An incomplete chunk must fail before the caller can accept the frame/cursor.
                server.enqueue(MockResponse().setBody(response(0, "x", null)))
                org.junit.Assert.assertTrue(
                    runCatching { api.readLargeEvents("/runs/run/stream", listOf(preview)) }
                        .isFailure
                )
            }
        }
}
