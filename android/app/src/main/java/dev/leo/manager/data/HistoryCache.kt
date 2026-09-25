package dev.leo.manager.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
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
    val savedAt: Long = System.currentTimeMillis(),
    val position: ReadingPosition? = null,
    val version: Int = 1,
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
    private val memory = linkedMapOf<String, CachedHistory>()
    private val written = mutableMapOf<String, Long>()
    private val dirty = mutableSetOf<String>()
    private val maxEntry = 4 * 1024 * 1024
    private val maxTotal = 20L * 1024 * 1024
    private val ttl = 7L * 24 * 60 * 60 * 1000

    fun key(origin: String, session: String, path: String): String =
        MessageDigest.getInstance("SHA-256")
            .digest("$origin\n$session\n$path".toByteArray())
            .joinToString("") { "%02x".format(it) }

    private fun valid(value: CachedHistory) =
        value.version == 1 &&
            value.history.startsWith("v1:") &&
            value.cursor >= 0 &&
            System.currentTimeMillis() - value.savedAt < ttl &&
            value.events.all { it.id > 0 && it.id <= value.cursor }

    suspend fun read(key: String): CachedHistory? =
        withContext(Dispatchers.IO) {
            lock.withLock {
                val value =
                    memory[key]
                        ?: runCatching {
                            val file = File(root, key)
                            require(file.length() in 1..(maxEntry + 64).toLong())
                            wireJson.decodeFromString<CachedHistory>(
                                String(unseal(key, file.readBytes()), Charsets.UTF_8)
                            )
                        }
                            .getOrNull()
                if (value == null || !valid(value)) {
                    removeLocked(key)
                    null
                } else {
                    memory.remove(key)
                    memory[key] = value
                    trimMemory()
                    value
                }
            }
        }

    suspend fun save(
        key: String,
        value: CachedHistory,
        force: Boolean = false,
        expectedGeneration: Long = generation,
    ) =
        withContext(Dispatchers.IO) {
            lock.withLock {
                if (expectedGeneration != generation) return@withLock
                if (!valid(value)) {
                    removeLocked(key)
                    return@withLock
                }
                val position =
                    (value.position
                            ?: memory[key]?.takeIf { it.history == value.history }?.position)
                        ?.takeIf {
                            it.firstEvent == null || it.firstEvent == value.events.firstOrNull()?.id
                        }
                val original = value.copy(position = position)
                val next = recentHistory(original)
                memory.remove(key)
                memory[key] = next
                dirty.add(key)
                trimMemory()
                if (force || System.currentTimeMillis() - (written[key] ?: 0) >= 1000)
                    writeLocked(key)
            }
        }

    suspend fun position(key: String, value: ReadingPosition) =
        withContext(Dispatchers.IO) {
            lock.withLock {
                memory[key]?.let {
                    if (value.firstEvent != null && value.firstEvent != it.events.firstOrNull()?.id)
                        return@withLock
                    memory[key] = it.copy(position = value)
                    dirty.add(key)
                }
            }
        }

    suspend fun flush(key: String) =
        withContext(Dispatchers.IO) { lock.withLock { writeLocked(key) } }

    suspend fun remove(key: String) =
        withContext(Dispatchers.IO) { lock.withLock { removeLocked(key) } }

    suspend fun clear() =
        withContext(Dispatchers.IO) {
            lock.withLock {
                generation++
                memory.clear()
                written.clear()
                dirty.clear()
                root.listFiles()?.forEach { it.delete() }
            }
        }

    private fun removeLocked(key: String) {
        memory.remove(key)
        written.remove(key)
        dirty.remove(key)
        AtomicFile(File(root, key)).delete()
    }

    private fun trimMemory() {
        var total = 0L
        for ((key, value) in memory.toList().asReversed()) {
            total +=
                value.events.sumOf {
                    (it.text.length + it.payload.toString().length).toLong() * 2
                } + wireJson.encodeToString(value.state).length.toLong() * 2
            if (total > maxTotal) {
                memory.remove(key)
                written.remove(key)
                dirty.remove(key)
            }
        }
        while (memory.size > 12) {
            val key = memory.keys.first()
            memory.remove(key)
            written.remove(key)
            dirty.remove(key)
        }
    }

    private fun writeLocked(key: String) {
        if (key !in dirty) return
        val value = memory[key] ?: return
        runCatching {
            val bytes = wireJson.encodeToString(value).toByteArray()
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
                .sortedByDescending { it.lastModified() }
                .forEachIndexed { index, file ->
                    total += file.length()
                    if (
                        index >= 12 ||
                            total > maxTotal ||
                            System.currentTimeMillis() - file.lastModified() >= ttl
                    )
                        file.delete()
                }
        } // Storage/key invalidation is a cache miss, never a failed conversation.
    }

    companion object {
        fun encrypted(context: Context): HistoryCache {
            fun key(): SecretKey {
                val alias = "leo.history.v1"
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
    val kept = ArrayDeque<RunEvent>()
    var bytes = wireJson.encodeToString(value.state).toByteArray().size + 4096
    for (event in value.events.sortedByDescending { it.id }) {
        val size = wireJson.encodeToString(event).toByteArray().size + 1
        if (kept.size >= 200 || bytes + size > 3 * 1024 * 1024) break
        kept.addFirst(event)
        bytes += size
    }
    if (kept.size == value.events.size) return value
    // A numeric list index is no longer valid after dropping a prefix.
    val oldest = kept.minOfOrNull { it.id } ?: (value.cursor + 1)
    return value.copy(
        events = value.events.filter { it.id >= oldest },
        oldest = oldest,
        hasOlder = true,
        position = null,
    )
}
