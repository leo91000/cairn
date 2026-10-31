package build.cairn.app.data

import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.contracts.LargeTimelineReference
import build.cairn.contracts.RunContentOutput
import build.cairn.contracts.TimelineContent
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LargeTimelineDeviceTest {
    @Test
    fun reconstructsAnOversizedReplyOnDevice() = runBlocking {
        val json =
            """{"id":"message:1:é","runId":"run","position":1,"revision":2,"source":2,"turn":1,"attempt":0,"createdAt":1,"updatedAt":2,"content":{"kind":"agentMessage","itemId":"é","text":"🦊 résultat","state":"done"}}"""
        val split = json.indexOf("🦊")
        val first = json.substring(0, split)
        val second = json.substring(split)
        val next = first.toByteArray(Charsets.UTF_8).size.toLong()
        val total = json.toByteArray(Charsets.UTF_8).size.toLong()
        val checksum = "a".repeat(64)
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
                        checksum,
                        total,
                    )
                )
            server.enqueue(MockResponse().setBody(response(0, first, next)))
            server.enqueue(MockResponse().setBody(response(next, second, null)))
            val vault =
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) = Unit
                }
            val api = CairnApi(server.url("/"), vault)
            val shell =
                wireJson.decodeFromString<TimelineCard>(
                    json.replace("🦊 résultat", "").replace("\"itemId\":\"é\"", "\"itemId\":\"\"")
                )
            val card =
                api.readLargeTimeline("/runs/run/stream", listOf(LargeTimelineReference(shell)))
                    .single()
            assertEquals("🦊 résultat", (card.content as TimelineContent.B).text)
            assertEquals(2, server.requestCount)
        }
    }
}
