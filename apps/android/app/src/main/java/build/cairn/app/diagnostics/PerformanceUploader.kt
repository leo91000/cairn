package build.cairn.app.diagnostics

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/** Shared for this process, including foreground entries, preference toggles and recreated UI. */
class UploadCadence(private val clock: () -> Long = System::nanoTime) {
    private var nextAttempt: Long? = null

    @Synchronized
    fun remainingMillis(): Long {
        val deadline = nextAttempt ?: return 0
        val remaining = (deadline - clock()).coerceAtLeast(0)
        return (remaining + 999_999) / 1_000_000
    }

    @Synchronized
    fun tryAcquire(): Boolean {
        if (remainingMillis() > 0) return false
        nextAttempt = clock() + 60_000_000_000L
        return true
    }
}

/** Each lifecycle collection may be cancelled; its shared monotonic deadline survives it. */
suspend fun runPerformanceUploads(cadence: UploadCadence, upload: suspend () -> Unit) {
    delay(10_000)
    while (currentCoroutineContext().isActive) {
        val remaining = cadence.remainingMillis()
        if (remaining > 0) {
            delay(remaining)
            continue
        }
        if (cadence.tryAcquire()) upload()
        delay(60_000)
    }
}

/** Cumulative windows are upserted by Beacon, so retries never double-count a session. */
class PerformanceUploader(
    private val send: suspend (String) -> Unit,
    private val snapshot: () -> PerformanceSnapshot = { Performance.store.snapshot() },
    private val report: (PerformanceSnapshot) -> String = Performance::report,
) {
    private var accepted: Pair<String, Long>? = null

    suspend fun upload() =
        withContext(Dispatchers.Default) {
            val data = snapshot()
            val revision = data.windowId to data.sequence
            if (data.sequence == 0L || revision == accepted) return@withContext
            try {
                // No disk queue, request replay loop or user-facing error when Beacon is
                // unavailable.
                val sent =
                    withTimeoutOrNull(5_000) {
                        send(report(data))
                        true
                    }
                if (sent == true) accepted = revision
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // The next scheduled attempt sends the latest cumulative window.
            }
        }
}
