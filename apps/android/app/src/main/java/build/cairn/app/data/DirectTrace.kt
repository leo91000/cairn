package build.cairn.app.data

import kotlinx.serialization.json.*

/** Opt-in qualification metadata. Never accepts wire data, URLs, IDs or exception messages. */
internal class DirectTrace(
    private val monotonicMs: () -> Long = { System.nanoTime() / 1_000_000 },
    private val wallMs: () -> Long = System::currentTimeMillis,
) {
    enum class Event {
        FOREGROUND,
        BACKGROUND,
        NETWORK_AVAILABLE,
        NETWORK_LOST,
        ATTEMPT_START,
        ICE_STATE,
        DATA_STATE,
        GRANT_UPDATED,
        GRANT_CAPPED,
        NETWORK_CHANGED,
        CLIENT_CLOSED,
        ATTEMPT_CLOSED,
        CONNECT_ERROR,
        CONNECT_TIMEOUT,
        ICE_FAILED,
        CANDIDATE_QUEUE_FULL,
        UNEXPECTED_MEDIA,
        UNEXPECTED_CHANNEL,
        DATA_CLOSED,
        OPEN_REJECTED,
        INVALID_PACKET,
        GRANT_EXPIRED,
        SIGNAL_ERROR,
        SIGNAL_ENDED,
        RENEW_ERROR,
        CHANNEL_CLOSED,
        VALIDITY_REJECTED,
        DECODE_ERROR,
        PROTOCOL_MISMATCH,
        RECEIVE_ERROR,
        ASSEMBLY_TIMEOUT,
        REQUEST_STARTED,
        REQUEST_NOT_SENT,
        REQUEST_LOST,
        REQUEST_TIMEOUT,
        REQUEST_IO_ERROR,
        RESPONSE_DIRECT,
        RESPONSE_RELAY,
        LARGE_REQUEST_BEFORE,
        LARGE_REQUEST_AFTER,
        ROUTE_DIRECT,
        ROUTE_RELAY,
    }

    private val started = monotonicMs()
    private val wallStarted = wallMs()
    private val entries = ArrayDeque<JsonObject>()
    private var dropped = 0L
    private var firstFailure: JsonObject? = null
    @Volatile var grantDeadlineMs: Long = 0

    // Enum names (including native ICE/data states) are the only text accepted by this recorder.
    @Synchronized
    fun record(
        event: Event,
        value: Long? = null,
        state: Enum<*>? = null,
        taggedDirect: Boolean? = null,
    ) {
        val elapsed = monotonicMs() - started
        if (entries.size == 128) {
            entries.removeFirst()
            dropped++
        }
        val wall = wallMs()
        val grantDeadline = grantDeadlineMs
        entries.addLast(
            buildJsonObject {
                put("event", event.name)
                put("elapsedMs", elapsed)
                put("wallMs", wall)
                put("wallDeltaMs", wall - wallStarted - elapsed)
                if (grantDeadline != 0L) put("grantRemainingMs", grantDeadline - wall)
                value?.let { put("value", it) }
                state?.let { put("state", it.name) }
                taggedDirect?.let { put("taggedDirect", it) }
            }
        )
    }

    @Synchronized
    fun failure(event: Event, value: Long? = null) {
        record(event, value)
        if (firstFailure == null) firstFailure = entries.last()
    }

    @Synchronized
    fun snapshot(): JsonObject = buildJsonObject {
        put("dropped", dropped)
        firstFailure?.let { put("firstFailure", it) }
        put("events", JsonArray(entries.toList()))
    }
}
