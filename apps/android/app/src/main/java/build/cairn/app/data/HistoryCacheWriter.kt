package build.cairn.app.data

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** One write in progress and one latest pending snapshot, without suspending the reader. */
internal class HistoryCacheWriter(
    private val cache: HistoryCache,
    private val key: String,
    scope: CoroutineScope,
) {
    private data class Write(val request: HistoryCache.PendingSave, val revision: Long)

    private val queue = Channel<Write>(Channel.CONFLATED)
    private val monitor = Any()
    private var revision = 0L
    private var closed = false
    private val completed = MutableStateFlow(0L)

    init {
        scope.launch(Dispatchers.IO, start = CoroutineStart.UNDISPATCHED) {
            var active: Write? = null
            suspend fun persist(write: Write) {
                try {
                    cache.persist(write.request)
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    // The cache is best effort; failure must not stop the live reader.
                    cache.discard(write.request)
                }
                completed.value = write.revision
            }
            try {
                for (write in queue) {
                    active = write
                    persist(write)
                    active = null
                }
            } finally {
                close()
                withContext(NonCancellable) {
                    try {
                        active?.let { persist(it) }
                        while (true) {
                            val write = queue.tryReceive().getOrNull() ?: break
                            persist(write)
                        }
                    } finally {
                        cache.flush(key)
                        completed.value = synchronized(monitor) { revision }
                    }
                }
            }
        }
    }

    fun offer(value: CachedHistory, expectedGeneration: Long): Boolean =
        synchronized(monitor) {
            if (closed) return@synchronized false
            val request =
                cache.prepareSave(key, value, expectedGeneration) ?: return@synchronized false
            val write = Write(request, ++revision)
            check(queue.trySend(write).isSuccess)
            true
        }

    suspend fun flush() {
        val target = synchronized(monitor) { revision }
        completed.first { it >= target }
        cache.flush(key)
    }

    fun close() =
        synchronized(monitor) {
            closed = true
            queue.close()
            Unit
        }
}
