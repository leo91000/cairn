package build.cairn.app.ui

import androidx.compose.ui.graphics.Color
import build.cairn.app.data.ChatAttachment
import build.cairn.app.data.fileSize
import java.text.NumberFormat
import java.util.Locale
import kotlinx.serialization.Serializable

/**
 * File families shared with the web app (`apps/web/src/attachments.ts`): the server only recognizes
 * images, so other uploads are classified by their extension.
 */
enum class AttachmentFamily(val color: Color) {
    IMAGE(Color(0xFF6D6A78)),
    PDF(Color(0xFFD2452F)),
    SHEET(Color(0xFF1F7A4D)),
    DOC(Color(0xFF2D62B3)),
    SLIDES(Color(0xFFC4581F)),
    CODE(Color(0xFF3B3A46)),
    TEXT(Color(0xFF8986A4)),
    ARCHIVE(Color(0xFFB9822A)),
    AUDIO(Color(0xFF7A4FB0)),
    VIDEO(Color(0xFFB23A6E)),
    OTHER(Color(0xFF6D6A78)),
}

private val familyExtensions =
    mapOf(
        AttachmentFamily.IMAGE to "png jpg jpeg gif webp svg heic heif avif bmp",
        AttachmentFamily.PDF to "pdf",
        AttachmentFamily.SHEET to "csv tsv xls xlsx ods numbers",
        AttachmentFamily.DOC to "doc docx odt rtf pages",
        AttachmentFamily.SLIDES to "ppt pptx odp key",
        AttachmentFamily.CODE to
            "js mjs cjs ts tsx jsx py rs kt kts java go rb php c h cc cpp hpp cs swift sh bash " +
                "zsh fish sql html htm css scss sass less vue svelte json jsonl yaml yml toml xml " +
                "ini gradle lua dart scala r ps1 dockerfile",
        AttachmentFamily.TEXT to "txt log md markdown rst",
        AttachmentFamily.ARCHIVE to "zip tar gz tgz bz2 xz 7z rar zst",
        AttachmentFamily.AUDIO to "mp3 wav m4a ogg oga flac aac opus",
        AttachmentFamily.VIDEO to "mp4 mov webm mkv avi m4v",
    )

private val familyByExtension =
    familyExtensions.flatMap { (family, list) -> list.split(' ').map { it to family } }.toMap()

private val extensionPattern = Regex("[a-z0-9]{1,8}")

/** The lowercased extension, when the name has a base before its last dot. */
fun attachmentExtension(name: String): String {
    val dot = name.lastIndexOf('.')
    if (dot <= 0) return ""
    val extension = name.substring(dot + 1).lowercase()
    return if (extensionPattern.matches(extension)) extension else ""
}

fun attachmentFamily(name: String, mediaType: String, kind: String = "file"): AttachmentFamily {
    val byExtension = familyByExtension[attachmentExtension(name)]
    return when {
        kind == "image" -> AttachmentFamily.IMAGE
        byExtension != null -> byExtension
        mediaType.startsWith("image/") -> AttachmentFamily.IMAGE
        mediaType == "application/pdf" -> AttachmentFamily.PDF
        mediaType.startsWith("audio/") -> AttachmentFamily.AUDIO
        mediaType.startsWith("video/") -> AttachmentFamily.VIDEO
        mediaType.startsWith("text/") -> AttachmentFamily.TEXT
        else -> AttachmentFamily.OTHER
    }
}

val ChatAttachment.family
    get() = attachmentFamily(name, mediaType, kind)

/** Badge text: the extension in capitals, at most four characters; empty shows a file icon. */
fun attachmentLabel(name: String): String = attachmentExtension(name).uppercase().take(4)

/** Document cards show a picture of the content: the first PDF page or the opening text. */
fun ChatAttachment.hasDocumentPreview(): Boolean =
    when (family) {
        AttachmentFamily.PDF,
        AttachmentFamily.CODE,
        AttachmentFamily.TEXT -> true
        AttachmentFamily.SHEET -> attachmentExtension(name) in setOf("csv", "tsv")
        else -> false
    }

/** What the server can tell about a file without downloading it (`?summary`). */
@Serializable
data class AttachmentSummary(
    val pages: Int? = null,
    val lines: Long? = null,
    val excerpt: String? = null,
)

private fun count(value: Long, one: String, many: String): String =
    "${NumberFormat.getIntegerInstance(Locale.FRANCE).format(value)} ${if (value == 1L) one else many}"

/** Page count of a PDF, line count of a table or text file, when the server knows it. */
fun attachmentDetail(attachment: ChatAttachment, summary: AttachmentSummary?): String? =
    when (attachment.family) {
        AttachmentFamily.PDF -> summary?.pages?.let { count(it.toLong(), "page", "pages") }
        AttachmentFamily.SHEET,
        AttachmentFamily.CODE,
        AttachmentFamily.TEXT -> summary?.lines?.let { count(it, "ligne", "lignes") }
        else -> null
    }

/** `PDF · 12 pages · 812 Ko`, with the details the summary provides. */
fun attachmentSubtitle(attachment: ChatAttachment, summary: AttachmentSummary? = null): String =
    listOfNotNull(
            attachmentLabel(attachment.name).ifEmpty { "Fichier" },
            attachmentDetail(attachment, summary),
            fileSize(attachment.size),
        )
        .joinToString(" · ")

/** Original or inferred file MIME for external save/share providers. */
fun attachmentMediaType(attachment: ChatAttachment): String =
    attachment.mediaType.takeUnless { it.isBlank() || it == "application/octet-stream" }
        ?: android.webkit.MimeTypeMap.getSingleton()
            .getMimeTypeFromExtension(attachmentExtension(attachment.name))
        ?: "application/octet-stream"

/** Kind and media type understood by the file viewer, which was written for artifacts. */
fun viewerType(attachment: ChatAttachment): Pair<String, String> {
    val extension = attachmentExtension(attachment.name)
    val kind =
        when (attachment.family) {
            AttachmentFamily.IMAGE -> "image"
            AttachmentFamily.PDF -> "pdf"
            AttachmentFamily.AUDIO -> "audio"
            AttachmentFamily.VIDEO -> "video"
            AttachmentFamily.TEXT ->
                if (extension in setOf("md", "markdown")) "markdown" else "code"
            AttachmentFamily.CODE -> "code"
            AttachmentFamily.SHEET -> if (extension in setOf("csv", "tsv")) "code" else "file"
            else -> "file"
        }

    // Android also maps .ts to MPEG transport streams; the chosen text family wins.
    val mediaType =
        when {
            kind == "code" || kind == "markdown" -> "text/plain"
            else -> attachmentMediaType(attachment)
        }
    return kind to mediaType
}

/** Up to four images show in the mosaic; the last tile counts the others. */
data class Mosaic(val shown: Int, val hidden: Int)

fun mosaic(count: Int): Mosaic = Mosaic(minOf(count, 4), (count - 4).coerceAtLeast(0))

/** Messages show images first, then document cards, then the other files. */
fun displayOrder(items: List<ChatAttachment>): List<ChatAttachment> =
    items.filter { it.family == AttachmentFamily.IMAGE } +
        items.filter { it.family != AttachmentFamily.IMAGE && it.hasDocumentPreview() } +
        items.filter { it.family != AttachmentFamily.IMAGE && !it.hasDocumentPreview() }
