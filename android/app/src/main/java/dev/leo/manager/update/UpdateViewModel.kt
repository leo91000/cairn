package dev.leo.manager.update

import android.app.Application
import android.content.Intent
import android.os.SystemClock
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class UpdateState(
    val available: AppUpdate? = null,
    val checking: Boolean = false,
    val downloading: Boolean = false,
    val installing: Boolean = false,
    val progress: Int = 0,
    val ready: File? = null,
    val showDialog: Boolean = false,
    val message: String? = null,
)

class UpdateViewModel(application: Application) : AndroidViewModel(application) {
    private val client = AppUpdateClient(application)
    private val mutable = MutableStateFlow(UpdateState())
    val state = mutable.asStateFlow()
    private var lastCheck: Long? = null

    fun check(manual: Boolean = false) {
        if (mutable.value.checking || mutable.value.downloading) return
        val now = SystemClock.elapsedRealtime()
        if (!manual && lastCheck?.let { now - it < 6 * 60 * 60 * 1000 } == true) return
        lastCheck = now
        mutable.update { it.copy(checking = true, message = null) }
        viewModelScope.launch {
            try {
                val update = client.check()
                mutable.update {
                    it.copy(
                        available = update,
                        ready = if (it.available == update) it.ready else null,
                        showDialog = update != null,
                        message =
                            if (manual && update == null) "L’application est à jour." else null,
                    )
                }
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                lastCheck = null
                if (manual) report(e)
            } finally {
                mutable.update { it.copy(checking = false) }
            }
        }
    }

    fun dismiss() {
        mutable.update { it.copy(showDialog = false) }
    }

    fun show() {
        mutable.update { it.copy(showDialog = true) }
    }

    fun report(e: Exception) {
        mutable.update { it.copy(message = e.message ?: "Mise à jour impossible. Réessayez.") }
    }

    fun download() {
        val update = mutable.value.available ?: return
        if (mutable.value.downloading) return
        mutable.update { it.copy(downloading = true, progress = 0, message = null) }
        viewModelScope.launch {
            try {
                val file =
                    client.download(update) { progress ->
                        mutable.update { it.copy(progress = progress) }
                    }
                mutable.update { it.copy(ready = file) }
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                report(e)
            } finally {
                mutable.update { it.copy(downloading = false) }
            }
        }
    }

    fun install(launch: (Intent) -> Unit) {
        val current = mutable.value
        if (current.installing) return
        val update = current.available ?: return
        val file = current.ready ?: return
        mutable.update { it.copy(installing = true, message = null) }
        viewModelScope.launch {
            try {
                launch(client.installationIntent(file, update))
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                // Android may evict cached APKs while the permission screen is open.
                // Offer a fresh download instead of repeatedly installing a missing file.
                mutable.update { it.copy(ready = null) }
                report(e)
            } finally {
                mutable.update { it.copy(installing = false) }
            }
        }
    }
}
