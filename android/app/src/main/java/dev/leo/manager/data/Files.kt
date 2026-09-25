package dev.leo.manager.data

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable

@Serializable
data class DraftAttachment(val attachment: ChatAttachment, val localPath: String? = null)

class Files(private val context: Context) {
    private val root
        get() = File(context.cacheDir, "leo-files").apply { mkdirs() }

    // `mediaType` covers pasted content whose provider does not report a type.
    suspend fun stage(uri: Uri, mediaType: String? = null): DraftAttachment =
        withContext(Dispatchers.IO) {
            val mime =
                context.contentResolver.getType(uri) ?: mediaType ?: "application/octet-stream"
            var name =
                if (mime.startsWith("image/"))
                    "image.${mime.substringAfter('/').substringBefore('+')}"
                else "pièce-jointe"
            context.contentResolver
                .query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
                ?.use {
                    if (it.moveToFirst()) name = it.getString(0).orEmpty().ifBlank { name }
                }
            val id = UUID.randomUUID().toString()
            val file = File(root, "$id-upload")
            try {
                var total = 0L
                requireNotNull(context.contentResolver.openInputStream(uri)) {
                        "Impossible de lire le fichier."
                    }
                    .use { input ->
                        file.outputStream().use { output ->
                            val buffer = ByteArray(65536)
                            while (true) {
                                val count = input.read(buffer)
                                if (count < 0) break
                                total += count
                                require(total <= 10L * 1024 * 1024) {
                                    "Chaque pièce jointe doit faire au maximum 10 Mo."
                                }
                                output.write(buffer, 0, count)
                            }
                        }
                    }
                DraftAttachment(
                    ChatAttachment(
                        id,
                        name = name,
                        size = total,
                        mediaType = mime,
                        kind =
                            if (
                                mime in listOf("image/png", "image/jpeg", "image/webp", "image/gif")
                            )
                                "image"
                            else "file",
                    ),
                    file.path,
                )
            } catch (e: Exception) {
                file.delete()
                throw e
            }
        }

    fun discard(items: List<DraftAttachment>) {
        items.forEach { it.localPath?.let { path -> File(path).delete() } }
    }

    suspend fun fetch(
        api: LeoApi,
        path: String,
        name: String,
        maxBytes: Long = 512L * 1024 * 1024,
    ): File =
        withContext(Dispatchers.IO) {
            val hash =
                MessageDigest.getInstance("SHA-256")
                    .digest((api.origin.toString() + path).toByteArray())
                    .joinToString("") { "%02x".format(it) }
            val safeName =
                name.replace(Regex("[^\\p{L}\\p{N}._ -]"), "_").takeLast(100).ifEmpty { "fichier" }
            val file = File(root, "$hash-$safeName")
            if (file.exists()) return@withContext file
            val partial = File(root, "${UUID.randomUUID()}.part")
            try {
                api.download(path, partial, maxBytes)
                if (!partial.renameTo(file)) {
                    check(file.exists()) { "Impossible de conserver le fichier." }
                    partial.delete()
                }
                file
            } catch (e: Exception) {
                partial.delete()
                throw e
            }
        }

    suspend fun save(file: File, uri: Uri) =
        withContext(Dispatchers.IO) {
            requireNotNull(context.contentResolver.openOutputStream(uri)) {
                    "Impossible d’enregistrer le fichier."
                }
                .use { output -> file.inputStream().use { it.copyTo(output) } }
        }
}

fun fileSize(size: Long): String =
    when {
        size < 1024 -> "$size o"
        size < 1024 * 1024 -> "${(size + 1023) / 1024} Ko"
        else -> String.format(java.util.Locale.FRANCE, "%.1f Mo", size / (1024.0 * 1024.0))
    }
