package dev.leo.manager.data

import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.channels.trySendBlocking
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.isActive
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import okhttp3.Call
import okhttp3.Callback
import okhttp3.Response

/** Wire compression is per connection; presentation keeps one row per assistant message. */
class LiveAccumulator {
    var cursor: Long = 0
        private set

    private var rows = mutableListOf<RunEvent>()
    private var messages = mutableMapOf<String, Int>()

    fun clear() {
        cursor = 0
        rows.clear()
        messages.clear()
    }

    fun restore(events: List<RunEvent>, accepted: Long) {
        clear()
        rows = events.toMutableList()
        cursor = accepted
        rows.forEachIndexed { index, event ->
            if (event.type == "turn.started") messages.clear()
            val item = event.payload?.get("item") as? JsonObject
            val id = item?.get("id")?.jsonPrimitive?.contentOrNull
            if (
                item?.get("type")?.jsonPrimitive?.contentOrNull == "agent_message" &&
                    !id.isNullOrBlank()
            )
                messages[id] = index
        }
    }

    fun append(incoming: List<RunEvent>): List<RunEvent> {
        // Commit only a complete valid batch, so reconnect never skips data after a parse failure.
        val next = rows.toMutableList()
        val indices = messages.toMutableMap()
        var accepted = cursor
        for (original in incoming) {
            if (original.id <= accepted) continue
            if (original.type == "turn.started") indices.clear()
            val item = original.payload?.get("item") as? JsonObject
            val id = item?.get("id")?.jsonPrimitive?.contentOrNull
            var event = original
            if (
                item?.get("type")?.jsonPrimitive?.contentOrNull == "agent_message" &&
                    !id.isNullOrBlank()
            ) {
                val index = indices[id]
                item["delta"]?.jsonPrimitive?.contentOrNull?.let { delta ->
                    require(index != null) { "Texte de référence manquant." }
                    val previous = next[index]
                    val oldItem = previous.payload?.get("item") as? JsonObject
                    val text =
                        (oldItem?.get("text")?.jsonPrimitive?.contentOrNull ?: previous.text) +
                            delta
                    val expanded =
                        JsonObject(
                            item.filterKeys { it != "delta" } + ("text" to JsonPrimitive(text))
                        )
                    event =
                        original.copy(
                            text = text,
                            payload = original.payload.orEmpty() + ("item" to expanded),
                        )
                }
                if (index == null) {
                    indices[id] = next.size
                    next.add(event)
                } else
                    next[index] =
                        event.copy(
                            createdAt = next[index].createdAt,
                            displayId = event.displayId ?: next[index].displayId ?: next[index].id,
                        )
            } else next.add(event)
            accepted = original.id
        }
        rows = next
        messages = indices
        cursor = accepted
        return next.toList()
    }
}

data class StreamFrame(val cursor: Long, val batch: LiveBatch)

data class LiveSnapshot(
    val state: LiveState? = null,
    val events: List<RunEvent> = emptyList(),
    val httpStatus: Int? = null,
    val status: String = "Connexion…",
    val error: String? = null,
    val catchingUp: Boolean = true,
    val history: String? = null,
    val cursor: Long = 0,
    val position: ReadingPosition? = null,
    val synced: Boolean = false,
    val oldest: Long = 0,
    val hasOlder: Boolean = false,
    val loadingOlder: Boolean = false,
    val olderError: String? = null,
    val loadOlder: () -> Unit = {},
)

private fun LeoApi.frames(
    path: String,
    cursor: Long,
    generation: Long,
    history: String?,
): Flow<StreamFrame> = callbackFlow {
    val call =
        streaming.newCall(
            builder(
                    "$path?after=$cursor" +
                        (history?.let { "&history=${segment(it)}" } ?: "") +
                        if (path == "/chats/stream") "" else "&window=1"
                )
                .header("Accept", "text/event-stream")
                .get()
                .build()
        )
    synchronized(streamLock) {
        if (generation != streamGeneration.get()) throw CancellationException("Session fermée")
        streamCalls.add(call)
    }
    call.enqueue(
        object : Callback {
            override fun onFailure(call: Call, e: IOException) {
                close(e)
            }

            override fun onResponse(call: Call, response: Response) {
                try {
                    response.use {
                        checkResponse(it)
                        require(
                            it.header("Content-Type").orEmpty().startsWith("text/event-stream")
                        ) {
                            "Flux temps réel invalide."
                        }
                        val source = it.body.source()
                        var event = ""
                        var id = ""
                        val data = StringBuilder()
                        while (!call.isCanceled() && !source.exhausted()) {
                            val line = source.readUtf8LineStrict(16L * 1024 * 1024)
                            if (line.isEmpty()) {
                                if (event == "batch") {
                                    val next =
                                        id.toLongOrNull()?.takeIf { n -> n >= 0 }
                                            ?: error("Curseur invalide")
                                    val batch =
                                        wireJson.decodeFromString<LiveBatch>(data.toString())
                                    if (trySendBlocking(StreamFrame(next, batch)).isFailure) return
                                }
                                event = ""
                                id = ""
                                data.clear()
                            } else {
                                val value = line.substringAfter(':', "").removePrefix(" ")
                                when (line.substringBefore(':')) {
                                    "event" -> event = value
                                    "id" -> id = value
                                    "data" -> {
                                        if (data.isNotEmpty()) data.append('\n')
                                        data.append(value)
                                        require(data.length <= 16 * 1024 * 1024) {
                                            "Événement trop volumineux."
                                        }
                                    }
                                }
                            }
                        }
                    }
                    close(IOException("Flux interrompu"))
                } catch (e: Exception) {
                    close(e)
                } finally {
                    streamCalls.remove(call)
                }
            }
        }
    )
    awaitClose {
        call.cancel()
        streamCalls.remove(call)
    }
}
    .buffer(1)

/** Retained by an open screen, while its network collection follows STARTED/STOPPED. */
class LiveSession {
    internal val mutex = Mutex()
    internal val accumulator = LiveAccumulator()
    internal var generation = -1L
    internal var path = ""
    internal var cursor = 0L
    internal var snapshot = LiveSnapshot()

    fun restore(value: CachedHistory, route: String, currentGeneration: Long) {
        accumulator.restore(value.events, value.cursor)
        generation = currentGeneration
        path = route
        cursor = value.cursor
        snapshot =
            LiveSnapshot(
                value.state,
                value.events,
                status = "Actualisation…",
                catchingUp = false,
                history = value.history,
                cursor = value.cursor,
                position = value.position,
                oldest = value.oldest,
                hasOlder = value.hasOlder,
            )
    }
}

fun LeoApi.live(path: String, session: LiveSession = LiveSession()): Flow<LiveSnapshot> = flow {
    session.mutex.withLock {
        val generation = streamGeneration.get()
        if (session.generation != generation || session.path != path) {
            session.accumulator.clear()
            session.cursor = 0L
            session.snapshot = LiveSnapshot()
            session.generation = generation
            session.path = path
        }
        val accumulator = session.accumulator
        var snapshot = session.snapshot
        var cursor = session.cursor
        // A collector may have stopped after parsing a frame but before displaying it.
        if (snapshot.state != null || snapshot.events.isNotEmpty()) emit(snapshot)
        var failures = 0
        while (currentCoroutineContext().isActive && generation == streamGeneration.get()) {
            try {
                frames(path, cursor, generation, snapshot.history).collect { frame ->
                    val batch = frame.batch
                    require(snapshot.history == null || batch.history != null) {
                        "Révision indisponible"
                    }
                    if (
                        batch.reset ||
                            (snapshot.history != null && batch.history != snapshot.history) ||
                            (batch.state != null && snapshot.state?.run?.id != batch.state.run?.id)
                    )
                        accumulator.clear()
                    val events = accumulator.append(batch.events)
                    snapshot =
                        LiveSnapshot(
                            batch.state ?: snapshot.state,
                            events,
                            status = "En direct",
                            catchingUp = batch.more,
                            synced = !batch.more,
                            history = batch.history,
                            cursor = frame.cursor,
                            oldest = batch.oldest ?: snapshot.oldest,
                            hasOlder =
                                if (batch.oldest != null) batch.hasOlder else snapshot.hasOlder,
                            position =
                                if (
                                    batch.reset ||
                                        (snapshot.history != null &&
                                            batch.history != snapshot.history)
                                )
                                    null
                                else snapshot.position,
                        )
                    cursor = frame.cursor
                    session.cursor = cursor
                    session.snapshot = snapshot
                    emit(snapshot)
                    failures = 0
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (generation != streamGeneration.get()) break
                val terminal = e is ApiException && e.status in setOf(401, 403, 404)
                if (e !is IOException) {
                    accumulator.clear()
                    cursor = 0
                    snapshot = LiveSnapshot()
                }
                snapshot =
                    snapshot.copy(
                        httpStatus = (e as? ApiException)?.status,
                        status = if (terminal) "Déconnecté" else "Reconnexion…",
                        error = if (terminal) e.message else null,
                    )
                session.cursor = cursor
                session.snapshot = snapshot
                emit(snapshot)
                if (terminal) break
            }
            delay((500L shl failures.coerceAtMost(5)).coerceAtMost(15000L))
            failures++
        }
    }
}
    .flowOn(Dispatchers.Default)
    // Only decoded, complete snapshots can be conflated. Never drop wire deltas.
    .conflate()

/** Prepending full snapshots must not replace a newer text revision at a page boundary. */
internal fun mergeHistory(older: List<RunEvent>, recent: List<RunEvent>): List<RunEvent> {
    val merged = (older + recent).distinctBy { it.id }.sortedBy { it.id }
    return LiveAccumulator().append(merged)
}
