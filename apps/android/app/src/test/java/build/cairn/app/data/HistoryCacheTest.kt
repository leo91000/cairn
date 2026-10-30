package build.cairn.app.data

import java.io.File
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class HistoryCacheTest {
    @get:Rule val directory = TemporaryFolder()

    private fun cache() = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })

    private fun record() =
        CachedHistory(
            2,
            "v2:r:1",
            LiveState(),
            listOf(
                RunEvent(
                    id = 2,
                    createdAt = 1,
                    type = "item.completed",
                    text = "saved",
                )
            ),
        )

    @Test
    fun `warm reads and neighboring saves reuse retained payload sizes`() = runBlocking {
        val traversals = AtomicInteger()
        val contents = mapOf<String, JsonElement>("output" to JsonPrimitive("retained output"))
        val payload =
            object : Map<String, JsonElement> by contents {
                override val entries: Set<Map.Entry<String, JsonElement>>
                    get() {
                        traversals.incrementAndGet()
                        return contents.entries
                    }
            }
        val cache = cache()
        cache.save(
            "retained",
            record()
                .copy(
                    events =
                        listOf(
                            RunEvent(
                                id = 2,
                                createdAt = 1,
                                type = "output",
                                text = "",
                                payload =
                                    (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
                            )
                        )
                ),
            true,
        )
        val initial = traversals.get()
        assertTrue(initial > 0)
        repeat(10) {
            assertNotNull(cache.read("retained"))
            cache.save("neighbor", record())
        }
        assertEquals(
            "Unchanged payloads must not be traversed for each cache access",
            initial,
            traversals.get(),
        )
    }

    @Test
    fun `previously loaded pages and reading position survive on disk beyond the initial window`() =
        runBlocking {
            val events =
                (1L..1250L).map {
                    RunEvent(
                        id = it,
                        createdAt = it,
                        type = "chat.user",
                        text = "Message $it",
                    )
                }
            val original =
                CachedHistory(
                    1250,
                    "v2:r:1",
                    LiveState(),
                    events,
                    position = ReadingPosition(5, 10, false),
                )
            val first = cache()
            first.save("window", original, true)
            val restored = cache().read("window")!!
            assertEquals(events.map { it.id }, restored.events.map { it.id })
            assertEquals(0L, restored.oldest)
            assertFalse(restored.hasOlder)
            assertEquals(ReadingPosition(5, 10, false), restored.position)
            assertEquals(1250L, restored.cursor)
        }

    @Test
    fun `the byte budget evicts only the oldest prefix and clears its reading position`() =
        runBlocking {
            val events =
                (1L..4L).map {
                    RunEvent(
                        id = it,
                        createdAt = it,
                        type = "output",
                        text = "x".repeat(1200 * 1024),
                    )
                }
            val first = cache()
            first.save(
                "bytes",
                CachedHistory(
                    4,
                    "v2:r:1",
                    LiveState(),
                    events,
                    position = ReadingPosition(5, 10, false),
                ),
                true,
            )
            val restored = cache().read("bytes")!!
            assertEquals(listOf(3L, 4L), restored.events.map { it.id })
            assertEquals(3L, restored.oldest)
            assertTrue(restored.hasOlder)
            assertNull(restored.position)
        }

    @Test
    fun `disk restores complete snapshots and position across instances and isolates sessions`() =
        runBlocking {
            val first = cache()
            val key = first.key("https://cairn.example", "session", "/chats/a/stream")
            first.save(key, record())
            first.position(key, ReadingPosition(12, 30, false))
            first.flush(key)
            val second = cache()
            assertEquals(record().events, second.read(key)?.events)
            assertEquals(ReadingPosition(12, 30, false), second.read(key)?.position)
            assertNull(second.read(second.key("https://cairn.example", "other", "/chats/a/stream")))
            second.save(key, record().copy(history = "v2:r:2"), true)
            assertNull(second.read(key)?.position)
            val previousGeneration = second.generation
            second.clear()
            second.save(key, record(), true, previousGeneration)
            assertNull(cache().read(key))
        }

    @Test
    fun `corruption expiry and oversized records fall back to an empty cache`() = runBlocking {
        val cache = cache()
        File(directory.root, "corrupt").writeText("broken")
        assertNull(cache.read("corrupt"))
        cache.save("expired", record().copy(savedAt = 1), true)
        assertNull(cache.read("expired"))
        cache.save("bad", record().copy(cursor = 1), true)
        assertNull(cache.read("bad"))
        cache.save(
            "huge",
            record()
                .copy(
                    events =
                        listOf(
                            RunEvent(
                                id = 2,
                                createdAt = 1,
                                type = "item.completed",
                                text = "x".repeat(4 * 1024 * 1024),
                            )
                        )
                ),
            true,
        )
        assertNotNull(cache.read("huge"))
        assertTrue(cache.read("huge")!!.hasOlder)
        assertEquals(3L, cache.read("huge")!!.oldest)
        for (i in 0..14) cache.save("key$i", record(), true)
        assertTrue(directory.root.listFiles()!!.size <= 12)
        assertNull(cache.read("key0"))
        assertNotNull(cache.read("key14"))
    }
}
