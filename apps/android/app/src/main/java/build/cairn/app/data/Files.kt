package build.cairn.app.data

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable

@Serializable
data class DraftAttachment(val attachment: ChatAttachment, val localPath: String? = null)

const val MAX_ATTACHMENT_BYTES = 10L * 1024 * 1024
const val MAX_MESSAGE_ATTACHMENT_BYTES = 40L * 1024 * 1024

/** Reuse keeps the existing record while enforcing the same draft limits as picked files. */
fun reuseAttachment(
    draft: List<DraftAttachment>,
    attachment: ChatAttachment,
): List<DraftAttachment> {
    if (draft.any { it.attachment.id == attachment.id }) return draft

    require(draft.size < 8) { "Vous pouvez joindre jusqu’à 8 fichiers." }
    require(draft.sumOf { it.attachment.size } + attachment.size <= MAX_MESSAGE_ATTACHMENT_BYTES) {
        "Les pièces jointes ne peuvent pas dépasser 40 Mo par message."
    }
    return draft + DraftAttachment(attachment)
}

/** A file refused before upload, shown in the composer until it is removed. */
@Serializable
data class RejectedAttachment(
    val id: String,
    val name: String,
    val mediaType: String,
    val reason: String,
)

class AttachmentRejected(val rejected: RejectedAttachment) : Exception(rejected.reason)

private fun tooLarge(size: Long?) =
    if (size == null) "Trop lourd (plus de 10 Mo)" else "Trop lourd (${fileSize(size)} > 10 Mo)"

class Files(private val context: Context) {
    private val cacheLock = Any()
    private var generation = 0L

    suspend fun clear() =
        withContext(Dispatchers.IO) {
            synchronized(cacheLock) {
                generation++
                File(context.cacheDir, "cairn-files").deleteRecursively()
            }
        }

    private val root
        get() = File(context.cacheDir, "cairn-files").apply { mkdirs() }

    // `mediaType` covers pasted content whose provider does not report a type.
    suspend fun stage(uri: Uri, mediaType: String? = null): DraftAttachment =
        withContext(Dispatchers.IO) {
            val expectedGeneration = synchronized(cacheLock) { generation }
            val mime =
                context.contentResolver.getType(uri) ?: mediaType ?: "application/octet-stream"
            var name =
                if (mime.startsWith("image/"))
                    "image.${mime.substringAfter('/').substringBefore('+')}"
                else "pièce-jointe"
            var reportedSize: Long? = null
            context.contentResolver
                .query(
                    uri,
                    arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                    null,
                    null,
                    null,
                )
                ?.use {
                    if (it.moveToFirst()) {
                        name = it.getString(0).orEmpty().ifBlank { name }
                        if (!it.isNull(1)) reportedSize = it.getLong(1)
                    }
                }
            val id = UUID.randomUUID().toString()
            fun rejected(size: Long?) =
                AttachmentRejected(RejectedAttachment(id, name, mime, tooLarge(size)))
            if ((reportedSize ?: 0) > MAX_ATTACHMENT_BYTES) throw rejected(reportedSize)
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
                                if (total > MAX_ATTACHMENT_BYTES) throw rejected(null)
                                output.write(buffer, 0, count)
                            }
                        }
                    }
                synchronized(cacheLock) {
                    if (generation != expectedGeneration) {
                        file.delete()
                        throw CancellationException("Le contexte de l’installation a changé.")
                    }
                }
                DraftAttachment(
                    ChatAttachment(
                        id = id,
                        name = name,
                        size = total.toInt(),
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
        api: CairnApi,
        path: String,
        name: String,
        maxBytes: Long = 512L * 1024 * 1024,
    ): File =
        withContext(Dispatchers.IO) {
            val expectedGeneration = synchronized(cacheLock) { generation }
            val hash =
                MessageDigest.getInstance("SHA-256")
                    .digest("${api.cacheScope}:${api.csrf}:$path".toByteArray())
                    .joinToString("") { "%02x".format(it) }
            val safeName =
                name.replace(Regex("[^\\p{L}\\p{N}._ -]"), "_").takeLast(100).ifEmpty { "fichier" }
            val file = File(root, "$hash-$safeName")
            if (file.exists()) return@withContext file
            val partial = File(root, "${UUID.randomUUID()}.part")
            try {
                api.download(path, partial, maxBytes)
                synchronized(cacheLock) {
                    if (generation != expectedGeneration) {
                        throw CancellationException("Le contexte de l’installation a changé.")
                    }
                    if (!partial.renameTo(file)) {
                        check(file.exists()) { "Impossible de conserver le fichier." }
                        partial.delete()
                    }
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
