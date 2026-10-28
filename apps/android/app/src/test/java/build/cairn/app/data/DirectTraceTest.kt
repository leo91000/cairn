package build.cairn.app.data

import build.cairn.app.data.DirectTrace.Event
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class DirectTraceTest {
    @Test
    fun `bounded trace retains the first failure after later cleanup and reads`() {
        val trace = DirectTrace()
        trace.failure(Event.DECODE_ERROR)
        trace.failure(Event.DATA_CLOSED)
        repeat(200) { trace.record(Event.RESPONSE_RELAY, 200) }
        val snapshot = trace.snapshot()
        assertEquals(128, snapshot["events"]!!.jsonArray.size)
        assertEquals(74, snapshot["dropped"]!!.jsonPrimitive.int)
        assertEquals(
            "DECODE_ERROR",
            snapshot["firstFailure"]!!.jsonObject["event"]!!.jsonPrimitive.content,
        )
    }

    @Test
    fun `wall clock jumps and grant remaining time do not change monotonic elapsed`() {
        var monotonic = 100L
        var wall = 10_000L
        val trace = DirectTrace({ monotonic }, { wall })
        trace.grantDeadlineMs = 190_000
        monotonic += 2_000
        wall += 62_000
        trace.record(Event.LARGE_REQUEST_BEFORE)
        val entry = trace.snapshot()["events"]!!.jsonArray.single().jsonObject
        assertEquals(2_000L, entry["elapsedMs"]!!.jsonPrimitive.long)
        assertEquals(60_000L, entry["wallDeltaMs"]!!.jsonPrimitive.long)
        assertEquals(118_000L, entry["grantRemainingMs"]!!.jsonPrimitive.long)
        assertEquals(72_000L, entry["wallMs"]!!.jsonPrimitive.long)
    }
}
