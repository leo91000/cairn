package build.cairn.app.data

import org.junit.Assert.*
import org.junit.Test

class DirectRetryTest {
    @Test
    fun `a successful connection resets the next failure to fifteen seconds`() {
        var now = 0L
        val retry = DirectRetry { now }
        retry.failed()
        assertEquals(15000L, retry.remaining())
        now = 15000L
        retry.failed()
        assertEquals(30000L, retry.remaining())
        now = 45000L
        retry.connected()
        retry.failed()
        assertEquals(15000L, retry.remaining())
    }

    @Test
    fun `foreground resumes only the remaining pending backoff`() {
        var now = 1000L
        val retry = DirectRetry { now }
        retry.failed()
        now = 6000L
        assertEquals(10000L, retry.remaining())
        now = 15999L
        assertEquals(1L, retry.remaining())
        now = 20000L
        assertEquals(0L, retry.remaining())
    }

    @Test
    fun `failures cap at five minutes and a network change clears the wait`() {
        var now = 0L
        val retry = DirectRetry { now }
        for (expected in listOf(15000L, 30000L, 60000L, 120000L, 240000L, 300000L, 300000L)) {
            retry.failed()
            assertEquals(expected, retry.remaining())
            now += expected
        }
        retry.failed()
        retry.networkChanged()
        assertEquals(0L, retry.remaining())
        retry.failed()
        assertEquals(15000L, retry.remaining())
    }
}
