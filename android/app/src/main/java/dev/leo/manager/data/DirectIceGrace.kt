package dev.leo.manager.data

import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** Only transient ICE disconnection gets a grace period; revocation never uses it. */
internal class DirectIceGrace(private val scope: CoroutineScope, private val fail: () -> Unit) {
    private val generation = AtomicLong()

    fun changed(disconnected: Boolean, terminal: Boolean = false) {
        val epoch = generation.incrementAndGet()
        if (terminal) fail()
        else if (disconnected)
            scope.launch {
                delay(5000)
                if (generation.get() == epoch) fail()
            }
    }
}
