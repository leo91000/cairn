package build.cairn.app.ui

import build.cairn.app.data.ApiException
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit

// Shared by listings, page paths and selected roots in every picker. A permit
// covers the whole generated call, including preparation polling.
private val resourcePreparationSlots = Semaphore(4)

internal suspend fun <T> withPluginResourcePreparation(prepare: suspend () -> T): T =
    resourcePreparationSlots.withPermit {
        retryResourcePreparation(prepare)
    }

private suspend fun <T> retryResourcePreparation(prepare: suspend () -> T): T {
    var attempt = 0
    while (true) {
        try {
            return prepare()
        } catch (error: ApiException) {
            // Other preparations may occupy slots. Authorization and plugin
            // failures remain actionable and are never retried here.
            if (error.status != 429 || attempt == 4) throw error
            delay(250L shl attempt)
            attempt++
        }
    }
}
