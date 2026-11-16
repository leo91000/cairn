package build.cairn.app.data

import android.content.Context
import android.os.StrictMode
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.Saver
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.serialization.json.*

/** Large editor text lives outside Binder; saved state contains only its scoped key. */
class RuleDraftState(draft: String = "", original: String = "", creation: String = "") {
    val draft = mutableStateOf(draft)
    val original = mutableStateOf(original)
    val creation = mutableStateOf(creation)
    val problem = mutableStateOf("")
    internal var closed = false
    internal var epoch = 0L
}

class RuleDraftStore(
    private val root: File,
    private val encode: (String, ByteArray) -> ByteArray = { _, bytes -> bytes },
    private val decode: (String, ByteArray) -> ByteArray = { _, bytes -> bytes },
) {
    private val memory =
        object : LinkedHashMap<String, RuleDraftState>(8, 0.75f, true) {
            override fun removeEldestEntry(
                eldest: MutableMap.MutableEntry<String, RuleDraftState>?
            ) = size > 8
        }
    private var generation = 0L
    private val maxBytes = 1024 * 1024
    private val maxSnapshots = 32
    private val maxAgeMillis = 7L * 24 * 60 * 60 * 1000
    private val snapshotName = Regex("[0-9a-f]{64}(\\.bak|\\.new)?")

    init {
        sweep()
    }

    /**
     * A saved-state token may survive process death, so retain recent snapshots with a fixed bound.
     */
    @Synchronized
    fun sweep() {
        runCatching { disk { prune() } }
    }

    private fun prune(keep: String? = null) {
        val cutoff = System.currentTimeMillis() - maxAgeMillis
        val groups =
            root
                .listFiles()
                .orEmpty()
                .filter { snapshotName.matches(it.name) }
                .groupBy { it.name.take(64) }
                .entries
                .sortedWith(
                    compareByDescending<Map.Entry<String, List<File>>> { it.key == keep }
                        .thenByDescending { entry -> entry.value.maxOf { it.lastModified() } }
                )
        groups.forEachIndexed { index, entry ->
            if (
                entry.key != keep &&
                    (index >= maxSnapshots || entry.value.all { it.lastModified() < cutoff })
            ) {
                AtomicFile(File(root, entry.key)).delete()
            }
        }
    }

    private fun key(scope: String, session: String) =
        MessageDigest.getInstance("SHA-256")
            .digest("$scope\u0000$session".toByteArray())
            .joinToString("") { "%02x".format(it) }

    @Synchronized
    fun fresh(scope: String, session: String): RuleDraftState =
        RuleDraftState().also {
            it.epoch = generation
            memory[key(scope, session)] = it
        }

    fun saver(scope: String, session: String): Saver<RuleDraftState, String> {
        val key = key(scope, session)
        val expectedGeneration = generation
        return Saver(
            save = { value -> save(key, expectedGeneration, value) },
            restore = { token ->
                if (token == key && expectedGeneration == generation) restore(key) else null
            },
        )
    }

    @Synchronized
    private fun save(key: String, expectedGeneration: Long, value: RuleDraftState): String? {
        if (value.closed || value.epoch != generation || expectedGeneration != generation)
            return null
        memory[key] = value
        try {
            check(
                value.draft.value.length +
                    value.original.value.length +
                    value.creation.value.length <= maxBytes
            ) {
                "Draft snapshot is too large."
            }
            val bytes = buildJsonObject {
                put("version", 1)
                put("draft", value.draft.value)
                put("original", value.original.value)
                put("creation", value.creation.value)
            }
                .toString()
                .toByteArray()
            val encoded = encode(key, bytes)
            check(encoded.size <= maxBytes) { "Draft snapshot is too large." }
            disk {
                if (!root.isDirectory && !root.mkdirs())
                    throw IOException("Draft directory unavailable.")
                val file = AtomicFile(File(root, key))
                val output = file.startWrite()
                try {
                    output.write(encoded)
                    file.finishWrite(output)
                    prune(key)
                } catch (error: Exception) {
                    file.failWrite(output)
                    throw error
                }
            }
            value.problem.value = ""
        } catch (_: Exception) {
            // The live ViewModel still retains this exact snapshot through recreation.
            value.problem.value =
                "Le brouillon n’a pas pu être conservé sur cet appareil. Copiez-le avant de quitter."
        }
        return key
    }

    @Synchronized
    private fun restore(key: String): RuleDraftState {
        memory[key]?.let {
            return it
        }
        val value =
            try {
                val bytes = disk {
                    AtomicFile(File(root, key)).openRead().use { input ->
                        val result = ByteArrayOutputStream()
                        val buffer = ByteArray(8192)
                        while (true) {
                            val count = input.read(buffer)
                            if (count < 0) break
                            if (result.size() + count > maxBytes)
                                throw IOException("Draft snapshot is too large.")
                            result.write(buffer, 0, count)
                        }
                        result.toByteArray()
                    }
                }
                val body =
                    wireJson.parseToJsonElement(decode(key, bytes).decodeToString()).jsonObject
                check(body["version"]?.jsonPrimitive?.intOrNull == 1)
                fun text(name: String) =
                    (body[name] as? JsonPrimitive)?.takeIf { it.isString }?.content
                        ?: throw IOException("Invalid draft snapshot.")
                RuleDraftState(text("draft"), text("original"), text("creation"))
            } catch (_: Exception) {
                RuleDraftState().apply {
                    problem.value =
                        "Le brouillon sauvegardé sur cet appareil n’est plus disponible."
                }
            }
        value.epoch = generation
        memory[key] = value
        return value
    }

    @Synchronized
    fun discard(scope: String, session: String, value: RuleDraftState) {
        value.closed = true
        val key = key(scope, session)
        memory.remove(key)
        runCatching { disk { AtomicFile(File(root, key)).delete() } }
    }

    @Synchronized
    fun clear() {
        generation++
        memory.values.forEach { it.closed = true }
        memory.clear()
        runCatching {
            disk {
                root.listFiles()?.filter { snapshotName.matches(it.name) }?.forEach { it.delete() }
            }
        }
    }

    private fun <T> disk(action: () -> T): T {
        val policy = StrictMode.allowThreadDiskWrites()
        return try {
            action()
        } finally {
            StrictMode.setThreadPolicy(policy)
        }
    }

    companion object {
        fun encrypted(
            context: Context,
            directory: File = File(context.noBackupFilesDir, "rule-drafts-v1"),
        ): RuleDraftStore {
            fun key(): SecretKey {
                val alias = "cairn.rule-drafts.v1"
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
            return RuleDraftStore(
                directory,
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
