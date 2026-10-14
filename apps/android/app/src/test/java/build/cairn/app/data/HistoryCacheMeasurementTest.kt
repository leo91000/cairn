package build.cairn.app.data

import java.lang.management.ManagementFactory
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** Local JVM file-cache costs; excludes Android Keystore and production network latency. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class HistoryCacheMeasurementTest {
    @get:Rule val directory = TemporaryFolder()

    @Test
    fun `measure a loaded conversation against the former 200 event window`() = runBlocking {
        val events =
            (1L..1250L).map {
                RunEvent(
                    it,
                    it,
                    "item.completed",
                    "Output $it\n" + "Synthetic output line.\n".repeat(20),
                )
            }
        val process =
            ManagementFactory.getOperatingSystemMXBean() as com.sun.management.OperatingSystemMXBean
        for (count in listOf(200, 1250)) {
            val root = directory.newFolder("events-$count")
            fun cache() = HistoryCache(root, { _, bytes -> bytes }, { _, bytes -> bytes })
            val value =
                CachedHistory(
                    1250,
                    "v1:measurement:1",
                    LiveState(),
                    events.takeLast(count),
                    oldest = events.takeLast(count).first().id,
                    hasOlder = count < 1250,
                )
            val cache = cache()
            repeat(5) { cache.save("measurement", value, force = true) }
            suspend fun median(action: suspend () -> Unit): Double {
                val times =
                    (1..30).map {
                        val start = System.nanoTime()
                        action()
                        (System.nanoTime() - start) / 1_000_000.0
                    }
                return times.sorted()[15]
            }
            val cpu = process.processCpuTime
            val write = median { cache.save("measurement", value, force = true) }
            val warm = median { cache.read("measurement") }
            val cold = median { cache().read("measurement") }
            assertEquals(count, cache().read("measurement")!!.events.size)
            println(
                "HISTORY_CACHE events=$count bytes=${root.resolve("measurement").length()} writeMs=$write warmMs=$warm coldMs=$cold processCpuMs=${(process.processCpuTime - cpu) / 1_000_000.0}"
            )
        }
    }
}
