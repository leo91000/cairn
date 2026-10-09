package dev.leo.manager.data

import android.os.SystemClock
import java.util.concurrent.TimeUnit

/** Retry deadlines outlive a foreground coroutine and use elapsed, not wall-clock, time. */
internal class DirectRetry(private val now: () -> Long = SystemClock::elapsedRealtime) {
    private var interval = 15000L
    private var retryAt: Long? = null

    @Synchronized fun remaining(): Long = retryAt?.let { maxOf(0L, it - now()) } ?: 0L

    @Synchronized
    fun failed() {
        retryAt = now() + interval
        interval = minOf(TimeUnit.MINUTES.toMillis(5), interval * 2)
    }

    @Synchronized
    fun connected() {
        interval = 15000L
        retryAt = null
    }

    @Synchronized
    fun networkChanged() {
        interval = 15000L
        retryAt = null
    }
}
