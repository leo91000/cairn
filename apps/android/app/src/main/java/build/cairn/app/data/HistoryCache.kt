package build.cairn.app.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import build.cairn.app.diagnostics.PerfOperation
import build.cairn.app.diagnostics.Performance
import java.io.File
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable

@Serializable
data class ReadingPosition(
    val index: Int = 0,
    val offset: Int = 0,
    val follow: Boolean = true,
    val firstEvent: Long? = null,
)

@Serializable
data class CachedHistory(
    val cursor: Long,
    val history: String,
    val state: LiveState,
    val events: List<RunEvent>,
    /** Timeline cards the server interpreted; never derived from the events. */
    val entries: List<TimelineCard> = emptyList(),
    val savedAt: Long = System.currentTimeMillis(),
    val position: ReadingPosition? = null,
    val version: Int = 2,
    val oldest: Long = 0,
    val hasOlder: Boolean = false,
)

/** Bounded, session-scoped snapshots. The cursor and decoded events commit together. */
class HistoryCache(
    private val root: File,
    private val seal: (String, ByteArray) -> ByteArray,
    private val unseal: (String, ByteArray) -> ByteArray,
) {
    @Volatile
    var generation: Long = 0
        private set

    private val lock = Mutex()

    private data class Entry(val value: CachedHistory, val bytes: Long)

    private data class Position(val history: String, val value: ReadingPosition)

    internal class PendingSave(val key: String, val value: CachedHistory, val generation: Long)

    private val memory = linkedMapOf<String, Entry>()
    private val pendingLock = Any()
    private val pending = mutableMapOf<String, PendingSave>()
    /** Deleted conversations: a reader still open never saves them again, as on the web. */
    private val forgotten = mutableSetOf<String>()
    private val positions = mutableMapOf<String, Position>()
    private val written = mutableMapOf<String, Long>()
    private val dirty = mutableSetOf<String>()
    private val maxEntry = 4 * 1024 * 1024
    private val maxTotal = 20L * 1024 * 1024
    private val ttl = 7L * 24 * 60 * 60 * 1000

    fun key(origin: String, session: String, path: String): String {
        val digest = digest("$origin\n$session\n$path")
        // Run logs are named apart, so they can be dropped without reading them.
        return if (path.startsWith("/runs/")) runLogPrefix(origin, session) + digest else digest
    }

    private fun runLogPrefix(origin: String, session: String) =
        "run-" + digest("$origin\n$session").take(16) + "-"

    private fun digest(value: String) =
        MessageDigest.getInstance("SHA-256").digest(value.toByteArray()).joinToString("") {
            "%02x".format(it)
        }

    private fun valid(value: CachedHistory) =
        value.version == 2 &&
            value.history.startsWith("v2:") &&
            value.cursor >= 0 &&
            System.currentTimeMillis() - value.savedAt < ttl &&
            value.events.all { it.id > 0 && it.id <= value.cursor }

    private fun olderThan(value: CachedHistory, reference: CachedHistory?) =
        reference?.history == value.history && reference.cursor > value.cursor

    suspend fun read(key: String): CachedHistory? =
        withContext(Dispatchers.IO) {
            Performance.measure(PerfOperation.CACHE_READ) {
                lock.withLock {
                    val retained = memory[key]?.value
                    val request = synchronized(pendingLock) { pending[key] }
                    val queued = request?.value?.takeUnless { olderThan(it, retained) }
                    val value =
                        queued?.let { prepareValue(key, it) }
                            ?: retained
                            ?: runCatching {
                                val file = File(root, key)
                                require(file.length() in 1..(maxEntry + 64).toLong())
                                wireJson
                                    .decodeFromString<CachedHistory>(
                                        String(unseal(key, file.readBytes()), Charsets.UTF_8)
                                    )
                                    .let { it.copy(state = it.state.withChatRun()) }
                            }
                                .getOrNull()
                    if (value == null || !valid(value)) {
                        request?.let { discard(it) }
                        removeLocked(key)
                        null
                    } else {
                        val previous = memory.remove(key)
                        memory[key] = if (previous?.value === value) previous else entry(value)
                        trimMemory()
                        value
                    }
                }
            }
        }

    internal fun prepareSave(
        key: String,
        value: CachedHistory,
        expectedGeneration: Long,
    ): PendingSave? =
        synchronized(pendingLock) {
            if (expectedGeneration != generation || key in forgotten) return@synchronized null
            if (olderThan(value, pending[key]?.value)) return@synchronized null
            PendingSave(key, value, expectedGeneration).also { pending[key] = it }
        }

    private fun isCurrent(request: PendingSave) =
        synchronized(pendingLock) {
            request.generation == generation && pending[request.key] === request
        }

    internal fun discard(request: PendingSave) =
        synchronized(pendingLock) {
            if (pending[request.key] === request) pending.remove(request.key)
            Unit
        }

    suspend fun save(
        key: String,
        value: CachedHistory,
        force: Boolean = false,
        expectedGeneration: Long = generation,
    ) {
        val request = prepareSave(key, value, expectedGeneration) ?: return
        try {
            persist(request, force)
        } finally {
            discard(request)
        }
    }

    internal suspend fun persist(request: PendingSave, force: Boolean = false) =
        withContext(Dispatchers.IO) {
            Performance.measure(PerfOperation.CACHE_SAVE) {
                lock.withLock {
                    if (!isCurrent(request)) return@withLock
                    val key = request.key
                    val value = request.value
                    try {
                        if (!valid(value)) {
                            removeLocked(key)
                            return@withLock
                        }
                        if (olderThan(value, memory[key]?.value)) return@withLock
                        val next = prepareValue(key, value)
                        memory.remove(key)
                        memory[key] = entry(next)
                        if (positions[key]?.history != next.history) positions.remove(key)
                        dirty.add(key)
                        trimMemory()
                        if (force || System.currentTimeMillis() - (written[key] ?: 0) >= 1000)
                            writeLocked(key)
                    } finally {
                        discard(request)
                    }
                }
            }
        }

    private fun prepareValue(key: String, value: CachedHistory): CachedHistory {
        val recent = recentHistory(value)
        val first = recent.events.firstOrNull()?.id
        val trimmed = recent !== value
        val position =
            (recent.position
                    ?: positions[key]?.takeIf { it.history == value.history }?.value
                    ?: memory[key]?.value?.takeIf { it.history == value.history }?.position)
                ?.takeIf {
                    if (it.firstEvent == null) !trimmed else it.firstEvent == first
                }
        return recent.copy(position = position)
    }

    suspend fun position(key: String, value: ReadingPosition) =
        withContext(Dispatchers.IO) {
            lock.withLock {
                val retained = memory[key]?.value
                val queued =
                    synchronized(pendingLock) { pending[key]?.value }
                        ?.takeUnless { olderThan(it, retained) }
                        ?.let { prepareValue(key, it) }
                val current = queued ?: retained ?: return@withLock
                if (
                    value.firstEvent != null && value.firstEvent != current.events.firstOrNull()?.id
                )
                    return@withLock
                positions[key] = Position(current.history, value)
                memory[key]?.let {
                    if (it.value.history == current.history) {
                        memory[key] = it.copy(value = it.value.copy(position = value))
                        dirty.add(key)
                    }
                }
            }
        }

    suspend fun flush(key: String) =
        withContext(Dispatchers.IO) { lock.withLock { writeLocked(key) } }

    /** Removes a deleted conversation's saved copy for good: it is never saved again. */
    suspend fun forget(key: String) {
        synchronized(pendingLock) { forgotten.add(key) }
        remove(key)
    }

    suspend fun remove(key: String) =
        withContext(Dispatchers.IO) {
            lock.withLock {
                synchronized(pendingLock) { pending.remove(key) }
                removeLocked(key)
            }
        }

    /**
     * Drops every saved run log of a session, by name. A run does not name its conversation, so a
     * deleted conversation's earlier runs cannot be told apart; a run log opens from its stream
     * again, as on the web.
     */
    suspend fun removeRunLogs(origin: String, session: String) =
        withContext(Dispatchers.IO) {
            val prefix = runLogPrefix(origin, session)
            lock.withLock {
                dropUnnamedRunLogs()
                val keys =
                    memory.keys.filter { it.startsWith(prefix) } +
                        synchronized(pendingLock) {
                            pending.keys.filter { it.startsWith(prefix) }
                        } +
                        root.list().orEmpty().filter { it.startsWith(prefix) }
                for (key in keys.toSet()) {
                    synchronized(pendingLock) { pending.remove(key) }
                    removeLocked(key)
                }
            }
        }

    /**
     * Run logs saved before they were named apart can only be told by reading them: this happens
     * once, at the first removal, and a marker records it.
     */
    private fun dropUnnamedRunLogs() {
        val marker = File(root, RUN_LOGS_NAMED)
        if (marker.exists()) return
        root
            .listFiles()
            .orEmpty()
            .filter { it.isFile && !it.name.startsWith("run-") && !it.name.startsWith(".") }
            .filter { file ->
                runCatching {
                        val value =
                            memory[file.name]?.value
                                ?: wireJson.decodeFromString<CachedHistory>(
                                    String(unseal(file.name, file.readBytes()), Charsets.UTF_8)
                                )
                        value.state.chat == null && value.state.run != null
                    }
                    .getOrDefault(false)
            }
            .forEach { removeLocked(it.name) }
        runCatching {
            root.mkdirs()
            marker.createNewFile()
        }
    }

    suspend fun clear() =
        withContext(Dispatchers.IO) {
            lock.withLock {
                synchronized(pendingLock) {
                    generation++
                    pending.clear()
                    forgotten.clear()
                }
                positions.clear()
                memory.clear()
                written.clear()
                dirty.clear()
                root.listFiles()?.forEach { it.delete() }
                // Nothing saved before run logs were named apart remains.
                runCatching {
                    root.mkdirs()
                    File(root, RUN_LOGS_NAMED).createNewFile()
                }
            }
        }

    private fun removeLocked(key: String) {
        memory.remove(key)
        positions.remove(key)
        // Passive cleanup preserves newer requests offered while a disk operation was in flight.
        written.remove(key)
        dirty.remove(key)
        AtomicFile(File(root, key)).delete()
    }

    private fun entry(value: CachedHistory): Entry {
        val bytes =
            value.events.sumOf {
                (it.text.length + it.payload.toString().length).toLong() * 2
            } +
                value.entries.sumOf { wireJson.encodeToString(it).length.toLong() * 2 } +
                wireJson.encodeToString(value.state).length.toLong() * 2
        return Entry(value, bytes)
    }

    private fun trimMemory() {
        var total = 0L
        for ((key, entry) in memory.toList().asReversed()) {
            total += entry.bytes
            if (total > maxTotal) evictMemory(key)
        }
        while (memory.size > MAX_COUNT) evictMemory(memory.keys.first())
    }

    private fun evictMemory(key: String) {
        memory.remove(key)
        positions.remove(key)
        written.remove(key)
        dirty.remove(key)
    }

    private fun writeLocked(key: String) {
        if (key !in dirty) return
        val value = memory[key]?.value ?: return
        runCatching {
            // The chat's run is the state's own `run`, restored when the file is read.
            val stored =
                value.copy(state = value.state.copy(chat = value.state.chat?.copy(run = null)))
            val bytes = wireJson.encodeToString(stored).toByteArray()
            if (bytes.size > maxEntry) {
                removeLocked(key)
                return
            }
            root.mkdirs()
            val atomic = AtomicFile(File(root, key))
            val stream = atomic.startWrite()
            try {
                stream.write(seal(key, bytes))
                atomic.finishWrite(stream)
            } catch (e: Exception) {
                atomic.failWrite(stream)
                throw e
            }
            written[key] = System.currentTimeMillis()
            dirty.remove(key)
            var total = 0L
            root
                .listFiles()
                .orEmpty()
                // Markers are not saved conversations.
                .filterNot { it.name.startsWith(".") }
                .sortedByDescending { it.lastModified() }
                .forEachIndexed { index, file ->
                    total += file.length()
                    if (
                        index >= MAX_COUNT ||
                            total > maxTotal ||
                            System.currentTimeMillis() - file.lastModified() >= ttl
                    )
                        file.delete()
                }
        } // Storage/key invalidation is a cache miss, never a failed conversation.
    }

    companion object {
        /** A saved conversation holds its own history only, so many fit the byte budget. */
        private const val MAX_COUNT = 60

        /** Marks a cache whose run logs are all named apart. */
        private const val RUN_LOGS_NAMED = ".run-logs-named"

        fun encrypted(context: Context): HistoryCache {
            fun key(): SecretKey {
                val alias = "cairn.history.v1"
                val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
                return (store.getKey(alias, null) as? SecretKey)
                    ?: KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
                        .apply {
                            init(
                                KeyGenParameterSpec.Builder(
                                        alias,
                                        KeyProperties.PURPOSE_ENCRYPT or
                                            KeyProperties.PURPOSE_DECRYPT,
                                    )
                                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                                    .build()
                            )
                        }
                        .generateKey()
            }
            return HistoryCache(
                File(context.noBackupFilesDir, "history-v1"),
                { scope, bytes ->
                    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                    cipher.init(Cipher.ENCRYPT_MODE, key())
                    cipher.updateAAD(scope.toByteArray())
                    cipher.iv + cipher.doFinal(bytes)
                },
                { scope, bytes ->
                    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
                    cipher.init(
                        Cipher.DECRYPT_MODE,
                        key(),
                        GCMParameterSpec(128, bytes.copyOfRange(0, 12)),
                    )
                    cipher.updateAAD(scope.toByteArray())
                    cipher.doFinal(bytes.copyOfRange(12, bytes.size))
                },
            )
        }
    }
}

/** Keep a contiguous recent suffix. An oversized older event stays available via pagination. */
internal fun recentHistory(value: CachedHistory): CachedHistory {
    val activeWaits =
        value.entries
            .filter { card ->
                val notice = (card.content as? build.cairn.contracts.TimelineContent.C)?.notice
                notice?.code == "backgroundTasks" && notice.waitingSince != null
            }
            .toSet()
    val groups = mutableMapOf<Long, Int>()
    for (event in value.events) {
        groups[event.id] =
            (groups[event.id] ?: 0) + wireJson.encodeToString(event).toByteArray().size + 1
    }
    for (card in value.entries) {
        if (card !in activeWaits) {
            groups[card.source] =
                (groups[card.source] ?: 0) + wireJson.encodeToString(card).toByteArray().size + 1
        }
    }
    var bytes =
        wireJson.encodeToString(value.state).toByteArray().size +
            4096 +
            activeWaits.sumOf { wireJson.encodeToString(it).toByteArray().size + 1 }
    val ordered = groups.keys.sortedDescending()
    var kept = 0
    for (id in ordered) {
        val size = groups.getValue(id)
        if (bytes + size > 3 * 1024 * 1024) break
        kept++
        bytes += size
    }
    if (kept == ordered.size) return value
    // A numeric list index is no longer valid after dropping a prefix.
    val oldest = if (kept == 0) value.cursor + 1 else ordered[kept - 1]
    return value.copy(
        events = value.events.filter { it.id >= oldest },
        entries = value.entries.filter { it.source >= oldest || it in activeWaits },
        oldest = oldest,
        hasOlder = true,
        position = null,
    )
}
