package build.cairn.app.data

import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
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

/**
 * A stream's events and timeline cards, with its replay cursor. Cards arrive interpreted by the
 * server: a newer revision replaces an older one, and a streaming reply may send only the text it
 * appends to the copy this stream delivered.
 */
class LiveAccumulator {
    var cursor: Long = 0
        private set

    private var rows = listOf<RunEvent>()
    private var cards = mapOf<String, TimelineCard>()
    private var streamCards = mapOf<String, TimelineCard>()

    val events: List<RunEvent>
        get() = rows

    val entries: List<TimelineCard>
        get() = timelineOrder(cards.values)

    fun clear() {
        cursor = 0
        rows = emptyList()
        cards = emptyMap()
        streamCards = emptyMap()
    }

    fun restore(events: List<RunEvent>, accepted: Long, entries: List<TimelineCard>) {
        rows = events.toList()
        cards = entries.associateBy { it.id }
        streamCards = cards
        cursor = accepted
    }

    /** Commits a complete valid batch only, so a reconnect never skips data after a failure. */
    fun append(
        incoming: List<RunEvent>,
        changes: List<TimelineChange> = emptyList(),
        accepted: Long = cursor,
    ) {
        val next = rows.toMutableList()
        var last = cursor
        for (event in incoming) {
            if (event.id <= last) continue
            next.add(event)
            last = event.id
        }
        val updated = cards.toMutableMap()
        val stream = streamCards.toMutableMap()
        for (change in changes) {
            when (change) {
                is build.cairn.contracts.TimelineChange.A -> {
                    stream[change.entry.id] = change.entry
                    upsert(updated, change.entry)
                }
                is build.cairn.contracts.TimelineChange.B -> {
                    val card = stream[change.id]
                    val reply = card?.content as? build.cairn.contracts.TimelineContent.B
                    require(card != null && reply != null) { "Texte de référence manquant." }
                    val next =
                        card.copy(
                            revision = change.revision,
                            source = change.source,
                            updatedAt = change.updatedAt,
                            content = reply.copy(text = reply.text + change.text),
                        )
                    stream[change.id] = next
                    upsert(updated, next)
                }
            }
        }
        rows = next
        cards = updated
        streamCards = stream
        cursor = maxOf(last, accepted)
    }
}

private fun upsert(cards: MutableMap<String, TimelineCard>, card: TimelineCard) {
    val current = cards[card.id]
    if (current == null || current.revision <= card.revision) cards[card.id] = card
}

/** Cards in the order they first appeared. */
internal fun timelineOrder(cards: Collection<TimelineCard>): List<TimelineCard> =
    cards.sortedWith(compareBy<TimelineCard> { it.position }.thenBy { it.id })

/** Joins an older page's cards; neither replaces a newer revision of the same card. */
internal fun mergeCards(older: List<TimelineCard>, recent: List<TimelineCard>): List<TimelineCard> {
    val cards = recent.associateByTo(mutableMapOf()) { it.id }
    for (card in older) upsert(cards, card)
    return timelineOrder(cards.values)
}

data class StreamFrame(val cursor: Long, val batch: LiveBatch)

/** A saved conversation is shown while its stream revalidates it. */
const val UPDATING_STATUS = "Actualisation…"

data class LiveSnapshot(
    val state: LiveState? = null,
    val events: List<RunEvent> = emptyList(),
    /** Timeline cards, in the order they first appeared. */
    val entries: List<TimelineCard> = emptyList(),
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
    /** Changes when the stream replaces its events; pages loaded above them are dropped. */
    val epoch: Int = 0,
    /** Tells the stream whether the reader follows the latest output, from the reading position. */
    val followLatest: (Boolean) -> Unit = {},
)

private fun CairnApi.frames(
    path: String,
    cursor: Long,
    generation: Long,
    history: String?,
    forceRelay: Boolean,
    recent: Boolean,
): Flow<StreamFrame> = callbackFlow {
    val windowed = path != "/chats/stream"
    val call =
        transport.newCall(
            streaming,
            builder(
                    "$path?after=$cursor" +
                        (history?.let { "&history=${segment(it)}" } ?: "") +
                        (if (windowed) "&window=1" else "") +
                        if (windowed && recent) "&recent=1" else ""
                )
                .header("Accept", "text/event-stream")
                .get()
                .build(),
            forceRelay,
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
                    val interrupted = IOException("Flux interrompu")
                    close(
                        if (transport.wasDirect(response)) DirectStreamLost(interrupted)
                        else interrupted
                    )
                } catch (e: Exception) {
                    close(
                        if (e is IOException && e !is ApiException && transport.wasDirect(response))
                            DirectStreamLost(e)
                        else e
                    )
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
    /**
     * A reader at the latest output resumes on the recent window instead of replaying a long
     * backlog; a reader of older messages keeps the replay that preserves their position.
     */
    @Volatile var followsLatest = true

    fun restore(value: CachedHistory, route: String, currentGeneration: Long) {
        accumulator.restore(value.events, value.cursor, value.entries)
        generation = currentGeneration
        path = route
        cursor = value.cursor
        followsLatest = value.position?.follow ?: true
        snapshot =
            LiveSnapshot(
                value.state,
                value.events,
                value.entries,
                status = UPDATING_STATUS,
                catchingUp = false,
                history = value.history,
                cursor = value.cursor,
                position = value.position,
                oldest = value.oldest,
                hasOlder = value.hasOlder,
            )
    }
}

suspend fun CairnApi.readLargeTimeline(
    path: String,
    previews: List<build.cairn.contracts.LargeTimelineReference>,
): List<TimelineCard> {
    val cards = mutableListOf<TimelineCard>()
    for (preview in previews) {
        val snapshot = preview.snapshot
        val text = StringBuilder()
        var offset = 0L
        var checksum: String? = null
        var total: Long? = null
        while (true) {
            val part =
                send<build.cairn.contracts.RunContentOutput>(
                    "POST",
                    path.removeSuffix("/stream") + "/timeline-content",
                    wireJson.encodeToJsonElement(
                        build.cairn.contracts.ReadLargeTimelineInput(
                            snapshot = snapshot,
                            offset = offset,
                            sha256 = checksum,
                        )
                    ),
                )
            require(
                part.entryId == snapshot.id &&
                    part.runId == snapshot.runId &&
                    part.offset == offset &&
                    (checksum == null || part.sha256 == checksum) &&
                    (total == null || part.totalBytes == total)
            ) {
                "Contenu de la chronologie modifié."
            }
            checksum = part.sha256
            total = part.totalBytes
            text.append(part.data)
            val end = offset + part.data.toByteArray(Charsets.UTF_8).size
            require(end <= part.totalBytes && (part.nextOffset ?: part.totalBytes) == end) {
                "Longueur de la chronologie invalide."
            }
            val next = part.nextOffset ?: break
            require(next > offset) { "Curseur de chronologie invalide." }
            offset = next
        }
        val card = wireJson.decodeFromString<TimelineCard>(text.toString())
        require(
            card.id == snapshot.id &&
                card.runId == snapshot.runId &&
                card.position == snapshot.position &&
                card.revision == snapshot.revision &&
                card.source == snapshot.source
        ) {
            "Carte de chronologie modifiée."
        }
        cards.add(card)
    }
    return cards
}

fun CairnApi.live(path: String, session: LiveSession = LiveSession()): Flow<LiveSnapshot> = flow {
    session.mutex.withLock {
        val generation = streamGeneration.get()
        if (session.generation != generation || session.path != path) {
            session.accumulator.clear()
            session.cursor = 0L
            session.snapshot = LiveSnapshot(epoch = session.snapshot.epoch + 1)
            session.generation = generation
            session.path = path
        }
        val accumulator = session.accumulator
        var snapshot = session.snapshot
        var cursor = session.cursor
        // A collector may have stopped after parsing a frame but before displaying it.
        if (snapshot.state != null || snapshot.events.isNotEmpty()) emit(snapshot)
        var failures = 0
        var relayRecovery = false
        val catchup = Performance.start(PerfOperation.LIVE_CATCHUP)
        var caughtUp = false
        while (currentCoroutineContext().isActive && generation == streamGeneration.get()) {
            try {
                val forceRelay = relayRecovery
                relayRecovery = false
                val recent = cursor > 0 && session.followsLatest
                frames(path, cursor, generation, snapshot.history, forceRelay, recent).collect {
                    frame ->
                    val batch = frame.batch
                    val large = readLargeTimeline(path, batch.largeEntries)
                    require(snapshot.history == null || batch.history != null) {
                        "Révision indisponible"
                    }
                    val reset =
                        batch.reset ||
                            (snapshot.history != null && batch.history != snapshot.history) ||
                            (batch.state != null && snapshot.state?.run?.id != batch.state.run?.id)
                    if (reset) accumulator.clear()
                    // A reconnect's recent window must not hide pages restored from disk.
                    val replaceBoundary =
                        batch.oldest != null &&
                            (reset || snapshot.oldest == 0L || batch.oldest <= snapshot.oldest)
                    Performance.measure(PerfOperation.LIVE_REDUCE) {
                        accumulator.append(
                            batch.events,
                            batch.entries +
                                large.map { build.cairn.contracts.TimelineChange.A(it) },
                            frame.cursor,
                        )
                    }
                    snapshot =
                        LiveSnapshot(
                            batch.state ?: snapshot.state,
                            accumulator.events,
                            accumulator.entries,
                            status = "En direct",
                            catchingUp = batch.more,
                            synced = !batch.more,
                            history = batch.history,
                            cursor = frame.cursor,
                            oldest =
                                if (replaceBoundary) batch.oldest!!
                                else if (reset) 0 else snapshot.oldest,
                            hasOlder =
                                if (replaceBoundary) batch.hasOlder
                                else !reset && snapshot.hasOlder,
                            position = if (reset) null else snapshot.position,
                            epoch = if (reset) snapshot.epoch + 1 else snapshot.epoch,
                        )
                    cursor = frame.cursor
                    if (!batch.more && !caughtUp) {
                        Performance.finish(catchup)
                        caughtUp = true
                    }
                    session.cursor = cursor
                    session.snapshot = snapshot
                    emit(snapshot)
                    failures = 0
                }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (generation != streamGeneration.get()) break
                if (e is DirectStreamLost) relayRecovery = true
                val terminal = e is ApiException && e.status in setOf(401, 403, 404)
                if (e !is IOException) {
                    accumulator.clear()
                    cursor = 0
                    snapshot = LiveSnapshot(epoch = snapshot.epoch + 1)
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

/** Prepending a full page keeps the newer copy of an event already loaded. */
internal fun mergeHistory(older: List<RunEvent>, recent: List<RunEvent>): List<RunEvent> {
    return Performance.measure(PerfOperation.HISTORY_MERGE) {
        (recent + older).distinctBy { it.id }.sortedBy { it.id }
    }
}
