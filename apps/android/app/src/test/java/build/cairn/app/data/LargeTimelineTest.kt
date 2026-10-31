package build.cairn.app.data

import build.cairn.contracts.RunContentOutput
import build.cairn.contracts.TimelineContent
import build.cairn.contracts.TimelinePreview
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
            val preview = TimelinePreview("message:1:é", "agentMessage", 1, 2, 2, "")
            val card = api.readLargeTimeline("/runs/run/stream", listOf(preview)).single()
            assertEquals("🦊 résultat", (card.content as TimelineContent.B).text)
            assertEquals("0", server.takeRequest().requestUrl!!.queryParameter("offset"))
            val secondRequest = server.takeRequest().requestUrl!!
            assertEquals(next.toString(), secondRequest.queryParameter("offset"))
            assertEquals(digest, secondRequest.queryParameter("sha256"))
        }
    }
}
