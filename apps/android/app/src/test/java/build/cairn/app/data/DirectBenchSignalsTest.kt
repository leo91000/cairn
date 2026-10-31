package build.cairn.app.data

import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import org.junit.Assert.*
import org.junit.Test

class DirectBenchSignalsTest {
    @Test
    fun `signal observation preserves every byte and exposes only bounded categories`() {
        val secret = "private-address-and-session"
        val payload =
            "event: signal\r\ndata: {\"kind\":\"answer\",\"sdp\":\"$secret\"}\r\n\r\n" +
                "event: signal\ndata: {\"kind\":\"candidate\",\"candidate\":\"$secret\"}\n\n" +
                "data: {\"kind\":\"$secret\"}\n\n" +
                "data: " +
                "x".repeat(20000) +
                "\n"
        val evidence = DirectBenchSignals()
        val original = payload.toResponseBody("text/event-stream".toMediaType())
        val body = evidence.observe(original)
        assertEquals(original.contentType(), body.contentType())
        assertEquals(payload.toByteArray().size.toLong(), body.contentLength())
        val delivered = Buffer()
        body.use {
            while (it.source().read(delivered, 3) != -1L) {}
        }
        assertEquals(payload, delivered.readUtf8())
        assertEquals("answers=1,candidates=1,malformed=2,closed=1", evidence.summary())
        assertFalse(evidence.summary().contains(secret))
    }
}
