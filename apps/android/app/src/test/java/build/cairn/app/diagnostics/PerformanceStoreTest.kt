package build.cairn.app.diagnostics

import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.junit.Assert.*
import org.junit.Test

class PerformanceStoreTest {
    @Test
    fun `frame deadlines follow refresh rate and swipe is separated from navigation`() {
        val store = PerformanceStore()
        store.frame(PerfScreen.CHAT, false, 12_000_000, 16_666_667)
        store.frame(PerfScreen.CHAT, true, 12_000_000, 8_333_333)
        store.frame(PerfScreen.CHAT, true, 700_000_000, 8_333_333, dropped = 3)
        store.frame(PerfScreen.CHAT, true, -1, 8_333_333)
        store.frame(PerfScreen.CHAT, true, 12_000_000, -1)
        val frames = store.snapshot().frames
        assertEquals(2, frames.size)
        assertEquals(0L, frames[0].missedDeadline)
        assertEquals(2L, frames[1].count)
        assertEquals(2L, frames[1].missedDeadline)
        assertEquals(1L, frames[1].frozen)
        assertEquals(3L, frames[1].droppedReports)
        assertEquals(700.0, frames[1].maxMs, 0.0)
    }

    @Test
    fun `operation statistics include main thread work and bounded percentile estimates`() {
        val store = PerformanceStore()
        repeat(95) { store.timing(PerfOperation.MARKDOWN, PerfScreen.CHAT, 10_000_000, true) }
        repeat(5) { store.timing(PerfOperation.MARKDOWN, PerfScreen.CHAT, 100_000_000, false) }
        val stat = store.snapshot().operations.single()
        assertEquals(100L, stat.count)
        assertEquals(95L, stat.startedOnMainThread)
        assertEquals(14.5, stat.meanMs, 0.0)
        assertEquals(100.0, stat.maxMs, 0.0)
        assertEquals(16.0, stat.p95UpperBoundMs, 0.0)
    }

    @Test
    fun `slow diagnostics stay bounded across a long session and reset clears measurements`() {
        var now = 0L
        val store = PerformanceStore { now }
        repeat(10_000) {
            now += 100_000_000
            store.timing(PerfOperation.JSON_DECODE, PerfScreen.FIL, 50_000_000, false)
        }
        val data = store.snapshot()
        assertEquals(1, data.operations.size)
        assertEquals(10_000L, data.operations.single().count)
        assertEquals(40, data.recentSlowSamples.size)
        assertEquals(996_100L, data.recentSlowSamples.first().elapsedMs)
        assertEquals(1_000_000L, data.elapsedMs)
        store.clear()
        assertEquals(0L, store.snapshot().elapsedMs)
        assertTrue(store.snapshot().operations.isEmpty())
        assertTrue(store.snapshot().frames.isEmpty())
        assertTrue(store.snapshot().recentSlowSamples.isEmpty())
    }

    @Test
    fun `frame and operation collectors can record concurrently without losing counts`() {
        val store = PerformanceStore()
        val workers = Executors.newFixedThreadPool(4)
        try {
            val tasks =
                (1..4).map {
                    workers.submit {
                        repeat(5_000) {
                            store.frame(PerfScreen.CHAT, true, 10_000_000, 16_666_667)
                            store.timing(PerfOperation.TIMELINE, PerfScreen.CHAT, 1_000_000, true)
                        }
                    }
                }
            for (task in tasks) task.get(10, TimeUnit.SECONDS)
            assertEquals(20_000L, store.snapshot().frames.single().count)
            assertEquals(20_000L, store.snapshot().operations.single().count)
        } finally {
            workers.shutdown()
        }
    }
}
