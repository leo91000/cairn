package build.cairn.app.data

import java.lang.management.ManagementFactory
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** Synthetic JVM measurements of the shipped cache, without Android Keystore or network. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class HistoryCacheCostDiagnosisTest {
    @get:Rule val directory = TemporaryFolder()

    private fun record(count: Int): CachedHistory {
        val events =
            (1L..count.toLong()).map { id ->
                val text = "Synthetic cache output. ".repeat(8)
                RunEvent(
                    id = id,
                    createdAt = id,
                    type = "item.completed",
                    text = text,
                    payload =
                        kotlinx.serialization.json.JsonObject(
                            mapOf(
                                "item" to
                                    buildJsonObject {
                                        put("id", "item-$id")
                                        put("type", "command_execution")
                                        put("aggregated_output", text)
                                        put("status", "completed")
                                    }
                            )
                        ),
                    runId = "fixture",
                )
            }
        return CachedHistory(count.toLong(), "v2:fixture:1", LiveState(), events)
    }

    @Test
    fun anotherConversationWarmReadWaitsForTheWriter() = runBlocking {
        assumeTrue(System.getenv("CAIRN_MEASURE_ANDROID_CACHE") == "1")
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val armed = AtomicBoolean(false)
        val cache =
            HistoryCache(
                directory.root,
                { _, bytes ->
                    if (armed.compareAndSet(true, false)) {
                        entered.countDown()
                        check(release.await(10, TimeUnit.SECONDS))
                    }
                    bytes
                },
                { _, bytes -> bytes },
            )
        val value = record(1)
        cache.save("other", value, true)
        assertNotNull(cache.read("other"))
        armed.set(true)
        val writer = async(Dispatchers.Default) { cache.save("busy", value, true) }
        try {
            assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
            val started = CompletableDeferred<Unit>()
            val reader =
                async(Dispatchers.Default) {
                    started.complete(Unit)
                    cache.read("other")
                }
            started.await()
            delay(100)
            assertFalse(
                "A warm read of another conversation is blocked by the writer",
                reader.isCompleted,
            )
            release.countDown()
            assertNotNull(withTimeout(5000) { reader.await() })
            writer.await()
            println("CACHE_LOCK_DIAGNOSIS unrelatedWarmReadBlocked=true releasedSuccessfully=true")
        } finally {
            release.countDown()
            writer.cancelAndJoin()
        }
    }

    @Test
    fun measureCachePreparationAndReads() = runBlocking {
        assumeTrue(System.getenv("CAIRN_MEASURE_ANDROID_CACHE") == "1")
        println(
            "CACHE_HOST cpus=${Runtime.getRuntime().availableProcessors()} java=${System.getProperty("java.version")} heap=${Runtime.getRuntime().maxMemory()}"
        )
        val thread = ManagementFactory.getThreadMXBean()
        for (count in listOf(200, 1250, 4000)) {
            val value = record(count)
            val encodedBytes = wireJson.encodeToString(value).toByteArray().size
            // Warm the serializer before measuring its repeated full-history traversal.
            repeat(3) { recentHistory(value) }
            val cpuStart = thread.currentThreadCpuTime
            val wallStart = System.nanoTime()
            repeat(10) { recentHistory(value) }
            val prepareWall = (System.nanoTime() - wallStart) / 1e6 / 10
            val prepareCpu = (thread.currentThreadCpuTime - cpuStart) / 1e6 / 10
            for (entries in listOf(1, 12)) {
                val root = directory.newFolder("events-$count-entries-$entries")
                fun cache() = HistoryCache(root, { _, bytes -> bytes }, { _, bytes -> bytes })
                val cache = cache()
                repeat(entries) { cache.save("key-$it", value, true) }
                suspend fun measure(block: suspend () -> Unit): Double {
                    val start = System.nanoTime()
                    block()
                    return (System.nanoTime() - start) / 1e6
                }
                val warm = mutableListOf<Double>()
                val cold = mutableListOf<Double>()
                val saves = mutableListOf<Double>()
                val writes = mutableListOf<Double>()
                repeat(8) {
                    warm += measure { assertNotNull(cache.read("key-${entries - 1}")) }
                    cold += measure { assertNotNull(cache().read("key-${entries - 1}")) }
                    // Within one second of the preceding forced write, so this still traverses
                    // the complete history and all retained entries but normally skips disk IO.
                    saves += measure { cache.save("key-${entries - 1}", value) }
                    writes += measure { cache.save("key-${entries - 1}", value, true) }
                }
                fun median(samples: List<Double>) = samples.sorted()[samples.size / 2]
                println(
                    buildJsonObject {
                        put("diagnostic", "CACHE_COST")
                        put("events", count)
                        put("retainedEntries", entries)
                        put(
                            "memoryEntries",
                            HistoryCache::class
                                .java
                                .getDeclaredField("memory")
                                .apply { isAccessible = true }
                                .get(cache)
                                .let { (it as Map<*, *>).size },
                        )
                        put("diskEntries", root.listFiles().orEmpty().size)
                        put("encodedBytes", encodedBytes)
                        put("diskBytes", root.listFiles().orEmpty().sumOf { it.length() })
                        put("recentHistoryWallMs", prepareWall)
                        put("recentHistoryCpuMs", prepareCpu)
                        put("warmReadP50Ms", median(warm))
                        put("coldReadP50Ms", median(cold))
                        put("saveP50Ms", median(saves))
                        put("forcedWriteP50Ms", median(writes))
                    }
                )
            }
        }
    }
}
