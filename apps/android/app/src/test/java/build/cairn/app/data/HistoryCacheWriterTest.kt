package build.cairn.app.data

import java.io.File
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.test.StandardTestDispatcher
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class HistoryCacheWriterTest {
    @get:Rule val directory = TemporaryFolder()

    private fun record(cursor: Long, history: String = "v1:fixture:1") =
        CachedHistory(
            cursor,
            history,
            LiveState(),
            listOf(
                RunEvent(
                    id = 1,
                    createdAt = 1,
                    type = "chat.user",
                    text = "Revision $cursor",
                    runId = "fixture",
                )
            ),
        )

    @Test
    fun aReopenedReaderSeesTheLatestQueuedSnapshotBeforeItReachesDisk() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        cache.save("chat", record(1), true)
        val request = cache.prepareSave("chat", record(2), cache.generation)!!
        assertEquals(2L, cache.read("chat")?.cursor)
        cache.position("chat", ReadingPosition(4, 18, false, 1))
        cache.persist(request)
        cache.flush("chat")
        val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        assertEquals(2L, disk.read("chat")?.cursor)
        assertEquals(ReadingPosition(4, 18, false, 1), disk.read("chat")?.position)
    }

    @Test
    fun positionsFromATrimmedQueuedSnapshotSurvivePersistence() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        val events =
            (1L..4L).map {
                RunEvent(
                    id = it,
                    createdAt = it,
                    type = "output",
                    text = "x".repeat(1200 * 1024),
                )
            }
        val request = cache.prepareSave("chat", record(4).copy(events = events), cache.generation)!!
        val restored = cache.read("chat")!!
        assertEquals(listOf(3L, 4L), restored.events.map { it.id })
        val position = ReadingPosition(1, 18, false, restored.events.first().id)

        cache.position("chat", position)
        cache.persist(request)
        cache.flush("chat")

        val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        assertEquals(position, disk.read("chat")?.position)
    }

    @Test
    fun staleCursorsCannotReplacePendingOrRetainedNewerOutput() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        val pending = cache.prepareSave("chat", record(10), cache.generation)!!
        assertNull(cache.prepareSave("chat", record(9), cache.generation))
        cache.persist(pending)
        cache.save("chat", record(9), true)
        cache.flush("chat")
        assertEquals(10L, cache.read("chat")?.cursor)
        cache.save("chat", record(1, "v1:new-run:1"), true)
        assertEquals(1L, cache.read("chat")?.cursor)
    }

    @Test
    fun readingAStaleQueuedSnapshotCannotRegressMemoryOrDisk() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        cache.save("chat", record(10), true)
        val stale = cache.prepareSave("chat", record(9), cache.generation)!!

        assertEquals(10L, cache.read("chat")?.cursor)
        cache.persist(stale)
        cache.flush("chat")

        val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        assertEquals(10L, disk.read("chat")?.cursor)
    }

    @Test
    fun aStaleQueuedBoundaryCannotDiscardTheRetainedReadingPosition() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        val events =
            (1L..10L).map {
                RunEvent(
                    id = it,
                    createdAt = it,
                    type = "chat.user",
                    text = "Message $it",
                )
            }
        cache.save("chat", record(10).copy(events = events), true)
        val stale =
            cache.prepareSave(
                "chat",
                record(9).copy(events = events.filter { it.id in 2L..9L }),
                cache.generation,
            )!!
        assertEquals(10L, cache.read("chat")?.cursor)
        val position = ReadingPosition(4, 18, false, 1)

        cache.position("chat", position)
        cache.persist(stale)
        cache.flush("chat")

        val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        assertEquals(position, disk.read("chat")?.position)
    }

    @Test
    fun expiredDiskCleanupPreservesASnapshotOfferedDuringTheRead() = runBlocking {
        File(directory.root, "chat").writeText(wireJson.encodeToString(record(1).copy(savedAt = 1)))
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val cache =
            HistoryCache(
                directory.root,
                { _, bytes -> bytes },
                { _, bytes ->
                    entered.countDown()
                    check(release.await(10, TimeUnit.SECONDS))
                    bytes
                },
            )
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val writer = HistoryCacheWriter(cache, "chat", scope)
        try {
            val reading = async(Dispatchers.IO) { cache.read("chat") }
            assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
            assertTrue(writer.offer(record(2), cache.generation))
            release.countDown()
            reading.await()
            withTimeout(5000) { writer.flush() }

            val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
            assertEquals(2L, disk.read("chat")?.cursor)
        } finally {
            release.countDown()
            writer.close()
            scope.cancel()
        }
    }

    @Test
    fun queuedUpdatesCoalesceAndCancellationPersistsTheLatestSnapshot() = runBlocking {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val saved = CopyOnWriteArrayList<Long>()
        val cache =
            HistoryCache(
                directory.root,
                { _, bytes ->
                    val value = wireJson.decodeFromString<CachedHistory>(bytes.decodeToString())
                    saved.add(value.cursor)
                    if (value.cursor == 1L) {
                        entered.countDown()
                        check(release.await(10, TimeUnit.SECONDS))
                    }
                    bytes
                },
                { _, bytes -> bytes },
            )
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val writer = HistoryCacheWriter(cache, "chat", scope)
        try {
            assertTrue(writer.offer(record(1), cache.generation))
            assertTrue(withContext(Dispatchers.IO) { entered.await(5, TimeUnit.SECONDS) })
            for (cursor in 2L..100L) assertTrue(writer.offer(record(cursor), cache.generation))
            scope.cancel()
            release.countDown()
            withTimeout(5000) { writer.flush() }
            val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
            assertEquals(100L, disk.read("chat")?.cursor)
            assertEquals(listOf(1L, 100L), saved.toList())
            assertFalse(writer.offer(record(101), cache.generation))
        } finally {
            release.countDown()
            scope.cancel()
        }
    }

    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun cancellationBetweenChannelHandoffAndResumePersistsTheAcceptedSnapshot() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        cache.save("chat", record(1), true)
        val dispatcher = StandardTestDispatcher()
        val scope = CoroutineScope(SupervisorJob() + dispatcher)
        val writer = HistoryCacheWriter(cache, "chat", scope, dispatcher)
        val worker = scope.coroutineContext[Job]!!.children.single()
        try {
            // UNDISPATCHED starts the receive; defer its resumption until after cancellation.
            assertTrue(writer.offer(record(2), cache.generation))
            scope.cancel()
            withTimeout(5000) {
                while (!worker.isCompleted) {
                    dispatcher.scheduler.runCurrent()
                    delay(1)
                }
                writer.flush()
            }

            val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
            assertEquals(2L, disk.read("chat")?.cursor)
        } finally {
            scope.cancel()
            dispatcher.scheduler.runCurrent()
        }
    }

    @Test
    fun anOlderReaderCannotOverwriteANewerReaderOrRecreateAClearedCache() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val old = HistoryCacheWriter(cache, "chat", scope)
        val current = HistoryCacheWriter(cache, "chat", scope)
        try {
            // Register in publication order, then persist the stale request after the new one.
            val stale = cache.prepareSave("chat", record(1), cache.generation)!!
            current.offer(record(2), cache.generation)
            current.flush()
            cache.persist(stale)
            assertEquals(2L, cache.read("chat")?.cursor)
            val generation = cache.generation
            val queued = cache.prepareSave("chat", record(3), generation)!!
            cache.clear()
            cache.persist(queued)
            assertFalse(old.offer(record(4), generation))
            old.close()
            old.flush()
            assertNull(cache.read("chat"))
            assertTrue(directory.root.listFiles().orEmpty().isEmpty())
        } finally {
            old.close()
            current.close()
            withContext(NonCancellable) {
                old.flush()
                current.flush()
            }
            scope.cancel()
        }
    }

    @Test
    fun readingPositionAndHistoryBoundarySurviveQueuedWritesAndInvalidation() = runBlocking {
        val cache = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val writer = HistoryCacheWriter(cache, "chat", scope)
        try {
            writer.offer(record(1), cache.generation)
            val position = ReadingPosition(4, 18, false, 1)
            cache.position("chat", position)
            writer.offer(record(2), cache.generation)
            writer.flush()
            val disk = HistoryCache(directory.root, { _, bytes -> bytes }, { _, bytes -> bytes })
            assertEquals(position, disk.read("chat")?.position)
            writer.offer(record(3, "v1:other:1"), cache.generation)
            writer.flush()
            assertNull(cache.read("chat")?.position)
            // A terminal removal invalidates work already registered by a reader.
            val pending = cache.prepareSave("chat", record(4), cache.generation)!!
            cache.remove("chat")
            cache.persist(pending)
            assertNull(cache.read("chat"))
        } finally {
            writer.close()
            withContext(NonCancellable) { writer.flush() }
            scope.cancel()
        }
    }
}
