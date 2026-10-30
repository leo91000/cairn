package build.cairn.app.data

import android.app.Activity
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

/** Bounded, category-only failure evidence; never retain account, request or session contents. */
internal class DirectBenchDiagnostics {
    private val started = System.nanoTime()
    private val events = mutableListOf<String>()
    private val latest = mutableMapOf<String, String>()
    private var omitted = 0
    val primaryRequests = AtomicInteger()
    val primaryResponses = AtomicInteger()
    val primaryDirectResponses = AtomicInteger()
    val primaryFailures = AtomicInteger()

    @Synchronized
    fun record(phase: String, state: String = "") {
        if (latest[phase] == state) return
        latest[phase] = state
        if (events.size == 64) {
            // Preserve startup as well as the most recent transitions.
            events.removeAt(16)
            omitted++
        }
        events.add(
            "${(System.nanoTime() - started) / 1_000_000}ms $phase $state " +
                "primary=${primaryRequests.get()}/${primaryResponses.get()}/" +
                "${primaryDirectResponses.get()}/${primaryFailures.get()}"
        )
    }

    fun workspace(vm: CairnViewModel?, installation: String, activity: Activity?) {
        if (vm == null) {
            record("workspace", "absent")
            return
        }
        val state = vm.state.value
        val api = runCatching { vm.api }.getOrNull()
        val peer =
            if (api == null) "absent"
            else
                runCatching {
                        val field = DirectTransport::class.java.getDeclaredField("current")
                        field.isAccessible = true
                        ((field.get(api.transport) as AtomicReference<*>).get() != null).toString()
                    }
                    .getOrDefault("unknown")
        val loading = if (!state.ready) "starting" else if (state.busy) "busy" else "idle"
        val route = state.transportRoute.takeIf { it in setOf("direct", "relay") } ?: "other"
        // These are instantaneous selected-VM HTTP calls, not cumulative responses. The VM
        // owns its default client; primary counters below cover only the separate bench SDK.
        val running = api?.http?.dispatcher?.runningCallsCount() ?: 0
        val queued = api?.http?.dispatcher?.queuedCallsCount() ?: 0
        record(
            "workspace",
            "authenticated=${state.session.authenticated} " +
                "selectedMatch=${state.installation?.id == installation} loading=$loading " +
                "online=${state.installation?.online} route=$route peerAttached=$peer " +
                "connectionLost=${state.connectionLost} error=${state.error != null} " +
                "protocolError=${state.protocolError != null} " +
                "httpRunning=$running httpQueued=$queued " +
                "process=${ProcessLifecycleOwner.get().lifecycle.currentState.name} " +
                "activity=${(activity as? LifecycleOwner)?.lifecycle?.currentState?.name} " +
                "destroyed=${activity?.isDestroyed}",
        )
    }

    @Synchronized
    fun summary() =
        "Direct bench phase evidence (omitted=$omitted; primary SDK requests=" +
            "${primaryRequests.get()}, responses=${primaryResponses.get()}, " +
            "directResponses=${primaryDirectResponses.get()}, failures=${primaryFailures.get()}):\n" +
            events.joinToString("\n")
}
