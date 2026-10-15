package build.cairn.app.diagnostics

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

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
