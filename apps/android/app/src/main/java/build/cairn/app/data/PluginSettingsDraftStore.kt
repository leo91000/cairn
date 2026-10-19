package build.cairn.app.data

import android.content.Context
import android.util.AtomicFile
import java.io.File
import java.io.IOException
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicLong

/** Scope-bound, bounded device-local snapshots of ordinary plugin setting inputs. */
internal class PluginSettingsDraftStore(
    context: Context,
    scope: String,
    private val allowed: Set<String>,
) {
    private val root = directory(context)
    private val epoch = generation.get()
    private val prefix =
        MessageDigest.getInstance("SHA-256").digest(scope.toByteArray()).joinToString("") {
            "%02x".format(it)
        }
    private val lifetime = 24 * 60 * 60 * 1000L
    private val maxBytes = 8 * 1024 * 1024L

    fun valid(key: String) =
        key.matches(Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"))

    private fun file(key: String) = File(root, "$prefix-$key.json")

    fun read(key: String): Map<String, String>? =
        synchronized(lock) {
            if (epoch != generation.get() || !valid(key)) return@synchronized null
            val file = file(key)
            try {
                AtomicFile(file).openRead().use { input ->
                    if (
                        input.channel.size() !in 1..maxBytes ||
                            file.lastModified() < System.currentTimeMillis() - lifetime
                    )
                        return@synchronized null
                    wireJson
                        .decodeFromString<Map<String, String>>(input.bufferedReader().readText())
                        .filterKeys { it in allowed }
                }
            } catch (_: IOException) {
                null
            } catch (_: kotlinx.serialization.SerializationException) {
                null
            }
        }

    fun write(key: String, values: Map<String, String>): Boolean =
        synchronized(lock) {
            if (epoch != generation.get() || !valid(key)) return@synchronized false
            val ordinary = values.filterKeys { it in allowed }
            if (ordinary.values.sumOf { it.length.toLong() } > maxBytes) return@synchronized false
            val bytes = wireJson.encodeToString(ordinary).toByteArray()
            if (bytes.size > maxBytes) return@synchronized false
            try {
                if (!root.exists() && !root.mkdirs()) return@synchronized false
                val file = file(key)
                prune(file, bytes.size.toLong())
                val atomic = AtomicFile(file)
                val output = atomic.startWrite()
                try {
                    output.write(bytes)
                    atomic.finishWrite(output)
                    true
                } catch (e: IOException) {
                    atomic.failWrite(output)
                    false
                }
            } catch (_: IOException) {
                false
            }
        }

    fun remove(key: String) =
        synchronized(lock) {
            if (epoch == generation.get() && valid(key)) AtomicFile(file(key)).delete()
        }

    private fun prune(current: File, incoming: Long) {
        val snapshots =
            root
                .listFiles()
                .orEmpty()
                .filter { it.isFile }
                .groupBy { it.name.removeSuffix(".bak").removeSuffix(".new") }
                .filterKeys { it.endsWith(".json") && it != current.name }
                .entries
                .sortedBy { entry -> entry.value.maxOf { it.lastModified() } }
        var size = incoming + snapshots.sumOf { entry -> entry.value.sumOf { it.length() } }
        var count = snapshots.size + 1
        val oldest = System.currentTimeMillis() - lifetime
        for ((name, files) in snapshots) {
            if (files.maxOf { it.lastModified() } < oldest || size > 2 * maxBytes || count > 32) {
                val bytes = files.sumOf { it.length() }
                AtomicFile(File(root, name)).delete()
                if (files.none { it.exists() }) {
                    size -= bytes
                    count--
                }
            }
        }
    }

    companion object {
        private val lock = Any()
        private val generation = AtomicLong()

        private fun directory(context: Context) =
            File(context.noBackupFilesDir, "plugin-setup-drafts")

        /** Retire open editors too, so a late state save cannot recreate signed-out data. */
        fun clear(context: Context) =
            synchronized(lock) {
                generation.incrementAndGet()
                directory(context).deleteRecursively()
                Unit
            }
    }
}
