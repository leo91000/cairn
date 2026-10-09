@file:OptIn(androidx.compose.foundation.ExperimentalFoundationApi::class)

package build.cairn.app.ui

import android.net.Uri
import androidx.compose.foundation.content.MediaType
import androidx.compose.foundation.content.TransferableContent
import androidx.compose.foundation.content.consume
import androidx.compose.foundation.content.hasMediaType

/** Images taken out of pasted or keyboard content; [rest] is left for the text field. */
class PastedImages(val uris: List<Uri>, val mediaType: String?, val rest: TransferableContent?)

fun pastedImages(content: TransferableContent): PastedImages {
    if (!content.hasMediaType(MediaType.Image)) return PastedImages(emptyList(), null, content)
    val description = content.clipMetadata.clipDescription
    val mediaType =
        (0 until description.mimeTypeCount)
            .map { description.getMimeType(it) }
            .firstOrNull { it.startsWith("image/") }
    val uris = mutableListOf<Uri>()
    val rest = content.consume { item -> item.uri?.also { uris += it } != null }
    return PastedImages(uris, mediaType, rest)
}
