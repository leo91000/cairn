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
}
