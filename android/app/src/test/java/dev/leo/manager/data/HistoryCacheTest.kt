package dev.leo.manager.data

import java.io.File
import kotlinx.coroutines.runBlocking
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
        CachedHistory(2, "v1:r:1", LiveState(), listOf(RunEvent(2, 1, "item.completed", "saved")))

    @Test
    fun `recent suffix survives on disk and records its backwards boundary`() = runBlocking {
        val events = (1L..250L).map { RunEvent(it, it, "chat.user", "Message $it") }
        val original =
            CachedHistory(
                250,
                "v1:r:1",
                LiveState(),
                events,
                position = ReadingPosition(5, 10, false),
            )
        val first = cache()
        first.save("window", original, true)
        val restored = cache().read("window")!!
        assertEquals((51L..250L).toList(), restored.events.map { it.id })
        assertEquals(51L, restored.oldest)
        assertTrue(restored.hasOlder)
        assertNull(restored.position)
        assertEquals(250L, restored.cursor)
    }

    @Test
    fun `disk restores complete snapshots and position across instances and isolates sessions`() =
        runBlocking {
            val first = cache()
            val key = first.key("https://leo.example", "session", "/chats/a/stream")
            first.save(key, record())
            first.position(key, ReadingPosition(12, 30, false))
            first.flush(key)
            val second = cache()
            assertEquals(record().events, second.read(key)?.events)
            assertEquals(ReadingPosition(12, 30, false), second.read(key)?.position)
            assertNull(second.read(second.key("https://leo.example", "other", "/chats/a/stream")))
            second.save(key, record().copy(history = "v1:r:2"), true)
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
                    events = listOf(RunEvent(2, 1, "item.completed", "x".repeat(4 * 1024 * 1024)))
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
