@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package build.cairn.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import android.net.Uri
import android.util.LruCache
import android.webkit.MimeTypeMap
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContract
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import build.cairn.app.data.*
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal fun attachmentPath(attachment: ChatAttachment) =
    "/chats/${segment(attachment.chatId)}/attachments/${segment(attachment.id)}"

/** Offers « Réutiliser » for files of the open conversation, which may be sent again as is. */
internal class AttachmentReuse(val chatId: String, val add: (ChatAttachment) -> Unit)

internal val LocalAttachmentReuse = staticCompositionLocalOf<AttachmentReuse?> { null }

// Decoded thumbnails survive recomposition and list recycling; files stay in the disk cache.
private val thumbnails =
    object : LruCache<String, Bitmap>(24 * 1024 * 1024) {
        override fun sizeOf(key: String, value: Bitmap) = value.byteCount
    }
private val summaries = ConcurrentHashMap<String, AttachmentSummary>()

@Composable
private fun attachmentCacheKey(vm: CairnViewModel, attachment: ChatAttachment): String {
    val workspace by vm.state.collectAsStateWithLifecycle()
    return wireJson.encodeToString(
        listOf(
            workspace.origin,
            workspace.installation?.id,
            workspace.session.account?.id,
            workspace.session.csrf,
            attachment.chatId,
            attachment.id,
        )
    )
}

internal fun decodeSampled(file: File, target: Int): Bitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeFile(file.path, bounds)
    if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
    val sample =
        Integer.highestOneBit((maxOf(bounds.outWidth, bounds.outHeight) / target).coerceAtLeast(1))
    val bitmap =
        BitmapFactory.decodeFile(file.path, BitmapFactory.Options().apply { inSampleSize = sample })
            ?: return null
    // Camera photos store their rotation separately from the pixels.
    val degrees = runCatching {
        when (
            ExifInterface(file.path)
                .getAttributeInt(
                    ExifInterface.TAG_ORIENTATION,
                    ExifInterface.ORIENTATION_NORMAL,
                )
        ) {
            ExifInterface.ORIENTATION_ROTATE_90 -> 90f
            ExifInterface.ORIENTATION_ROTATE_180 -> 180f
            ExifInterface.ORIENTATION_ROTATE_270 -> 270f
            else -> 0f
        }
    }
        .getOrDefault(0f)
    if (degrees == 0f) return bitmap
    return Bitmap.createBitmap(
        bitmap,
        0,
        0,
        bitmap.width,
        bitmap.height,
        Matrix().apply { postRotate(degrees) },
        true,
    )
}

/**
 * A small picture of an attachment: the local file before upload, else the server preview (an image
 * or the first PDF page), else the original image.
 */
private suspend fun loadThumbnail(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    localPath: String?,
    target: Int,
    key: String,
): Bitmap? {
    thumbnails.get(key)?.let {
        return it
    }
    val api = if (localPath == null && attachment.chatId.isNotBlank()) vm.api else null
    val image = attachment.family == AttachmentFamily.IMAGE
    val bitmap =
        withContext(Dispatchers.IO) {
            val file =
                when {
                    localPath != null -> if (image) File(localPath) else null
                    attachment.chatId.isBlank() -> null
                    else ->
                        try {
                            vm.files.fetch(
                                checkNotNull(api),
                                "${attachmentPath(attachment)}?preview",
                                "preview-${attachment.id}.jpg",
                                MAX_ATTACHMENT_BYTES,
                            )
                        } catch (e: CancellationException) {
                            throw e
                        } catch (_: Exception) {
                            if (image) fetchOriginal(vm.files, checkNotNull(api), attachment)
                            else null
                        }
                }
            file?.let { decodeSampled(it, target) }
        }
    bitmap?.let { thumbnails.put(key, it) }
    return bitmap
}

@Composable
private fun rememberThumbnail(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    localPath: String? = null,
    target: Dp = 128.dp,
): Bitmap? {
    val pixels = with(LocalDensity.current) { target.roundToPx() }
    val cacheKey = "${attachmentCacheKey(vm, attachment)}/$localPath/$pixels"
    return key(cacheKey) {
        produceState(thumbnails.get(cacheKey), cacheKey) {
                if (value == null)
                    value =
                        try {
                            loadThumbnail(vm, attachment, localPath, pixels, cacheKey)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (_: Exception) {
                            /* The file stays available from the viewer. */
                            null
                        }
            }
            .value
    }
}

/** Page or line counts and the opening text, from `?summary`; null until known. */
@Composable
private fun rememberSummary(vm: CairnViewModel, attachment: ChatAttachment): AttachmentSummary? {
    val wanted =
        attachment.chatId.isNotBlank() &&
            attachment.family in
                setOf(
                    AttachmentFamily.PDF,
                    AttachmentFamily.SHEET,
                    AttachmentFamily.CODE,
                    AttachmentFamily.TEXT,
                )
    val cacheKey = attachmentCacheKey(vm, attachment)
    return key(cacheKey) {
        produceState(summaries[cacheKey], cacheKey, wanted) {
                if (value != null || !wanted) return@produceState
                value =
                    try {
                        val file =
                            vm.files.fetch(
                                vm.api,
                                "${attachmentPath(attachment)}?summary",
                                "summary-${attachment.id}.json",
                                256L * 1024,
                            )
                        withContext(Dispatchers.IO) {
                                wireJson.decodeFromString<AttachmentSummary>(file.readText())
                            }
                            .also { summaries[cacheKey] = it }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (_: Exception) {
                        null
                    }
            }
            .value
    }
}

/** The coloured tile carrying the extension; a file icon when the name has none. */
@Composable
internal fun FileBadge(name: String, family: AttachmentFamily, modifier: Modifier = Modifier) {
    val label = attachmentLabel(name)
    Box(
        modifier.clip(RoundedCornerShape(7.dp)).background(family.color),
        contentAlignment = Alignment.Center,
    ) {
        if (label.isEmpty()) Icon(CairnIcons.File, null, Modifier.size(18.dp), tint = Color.White)
        else
            Text(
                label,
                color = Color.White,
                fontWeight = FontWeight.ExtraBold,
                fontSize = if (label.length > 3) 9.sp else 10.sp,
                maxLines = 1,
            )
    }
}

/** Keeps the extension in sight when a long name is shortened. */
@Composable
private fun AttachmentName(
    name: String,
    style: androidx.compose.ui.text.TextStyle,
    color: Color = Color.Unspecified,
) {
    val extension = attachmentExtension(name)
    val suffix = if (extension.isEmpty()) "" else name.takeLast(extension.length + 1)
    Row(Modifier.clearAndSetSemantics { text = AnnotatedString(name) }) {
        Text(
            name.dropLast(suffix.length),
            Modifier.weight(1f, fill = false),
            style = style,
            fontWeight = FontWeight.SemiBold,
            color = color,
            maxLines = 1,
            overflow = TextOverflow.MiddleEllipsis,
        )
        if (suffix.isNotEmpty())
            Text(
                suffix,
                style = style,
                fontWeight = FontWeight.SemiBold,
                color = color,
                // Reserve the extension first; allow wrapping when enlarged text needs it.
            )
    }
}

@Composable
private fun AttachmentPicture(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    modifier: Modifier,
    localPath: String? = null,
    target: Dp = 128.dp,
    alignment: Alignment = Alignment.Center,
) {
    val bitmap = rememberThumbnail(vm, attachment, localPath, target)
    Box(modifier.background(MaterialTheme.colorScheme.surfaceVariant)) {
        if (bitmap != null)
            Image(
                bitmap.asImageBitmap(),
                null,
                Modifier.matchParentSize(),
                contentScale = ContentScale.Crop,
                alignment = alignment,
            )
        else
            Icon(
                CairnIcons.Image,
                null,
                Modifier.align(Alignment.Center).size(22.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
    }
}

/** A file in one line: badge, shortened name, then type, details and size. */
@Composable
private fun FileCard(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    modifier: Modifier,
    subtitle: String? = null,
    error: Boolean = false,
) {
    val summary = if (subtitle == null) rememberSummary(vm, attachment) else null
    Surface(
        modifier,
        shape = RoundedCornerShape(12.dp),
        color =
            if (error) MaterialTheme.colorScheme.errorContainer
            else MaterialTheme.colorScheme.surface,
        border =
            BorderStroke(
                1.dp,
                if (error) MaterialTheme.colorScheme.error.copy(alpha = 0.5f)
                else MaterialTheme.colorScheme.outlineVariant,
            ),
    ) {
        Row(
            Modifier.fillMaxSize().padding(horizontal = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            FileBadge(
                attachment.name,
                attachment.family,
                Modifier.size(width = 34.dp, height = 40.dp),
            )
            Column(Modifier.weight(1f)) {
                AttachmentName(attachment.name, MaterialTheme.typography.labelLarge)
                Text(
                    subtitle ?: attachmentSubtitle(attachment, summary),
                    style = MaterialTheme.typography.labelSmall,
                    color =
                        if (error) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

// ---- A: composer tray --------------------------------------------------------------------

/** Attachments of the message being written, in one horizontal row above the text field. */
@Composable
internal fun ComposerAttachments(
    vm: CairnViewModel,
    items: List<DraftAttachment>,
    rejected: List<RejectedAttachment>,
    progress: Map<String, Float>,
    errors: Map<String, String>,
    remove: (String) -> Unit,
    dismiss: (String) -> Unit,
) {
    var opening by remember { mutableStateOf<ChatAttachment?>(null) }
    LazyRow(
        Modifier.fillMaxWidth().testTag("composer-attachments"),
        contentPadding = PaddingValues(horizontal = 8.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        itemsIndexed(items, key = { _, item -> item.attachment.id }) { _, item ->
            val attachment = item.attachment
            val uploading = progress[attachment.id]
            val error = errors[attachment.id]
            TrayItem(attachment.name, { remove(attachment.id) }) {
                val shape = RoundedCornerShape(12.dp)
                val outline =
                    if (error != null) Modifier.border(2.dp, MaterialTheme.colorScheme.error, shape)
                    else Modifier
                Box(
                    Modifier.clip(shape)
                        .then(outline)
                        .clickable(onClickLabel = "Aperçu") { opening = attachment }
                        .semantics {
                            contentDescription =
                                if (error == null) attachment.name
                                else "${attachment.name} : échec de l’envoi, $error"
                        }
                ) {
                    if (attachment.family == AttachmentFamily.IMAGE)
                        AttachmentPicture(
                            vm,
                            attachment,
                            Modifier.size(64.dp),
                            item.localPath,
                        )
                    else
                        FileCard(
                            vm,
                            attachment,
                            Modifier.size(width = 176.dp, height = 64.dp),
                            subtitle =
                                error?.let { "Échec de l’envoi · $it" }
                                    ?: attachmentSubtitle(attachment),
                            error = error != null,
                        )
                    if (uploading != null)
                        Box(
                            Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.45f)),
                            contentAlignment = Alignment.Center,
                        ) {
                            CircularProgressIndicator(
                                progress = { uploading },
                                modifier =
                                    Modifier.size(26.dp).semantics {
                                        contentDescription =
                                            "Envoi de ${attachment.name} : ${(uploading * 100).toInt()} %"
                                    },
                                color = Color.White,
                                trackColor = Color.White.copy(alpha = 0.3f),
                                strokeWidth = 3.dp,
                            )
                        }
                    else if (error != null && attachment.family == AttachmentFamily.IMAGE)
                        Box(
                            Modifier.matchParentSize()
                                .background(MaterialTheme.colorScheme.error.copy(alpha = 0.35f)),
                            contentAlignment = Alignment.Center,
                        ) {
                            Icon(CairnIcons.Alert, null, Modifier.size(22.dp), tint = Color.White)
                        }
                }
            }
        }
        itemsIndexed(rejected, key = { _, item -> "rejected:${item.id}" }) { _, item ->
            TrayItem(item.name, { dismiss(item.id) }) {
                FileCard(
                    vm,
                    ChatAttachment(id = item.id, name = item.name, mediaType = item.mediaType),
                    Modifier.size(width = 176.dp, height = 64.dp).semantics {
                        contentDescription = "${item.name} refusé : ${item.reason}"
                    },
                    subtitle = item.reason,
                    error = true,
                )
            }
        }
    }
    opening?.let { initial ->
        AttachmentGallery(
            vm,
            items.map { it.attachment },
            initial,
            items.mapNotNull { item -> item.localPath?.let { item.attachment.id to it } }.toMap(),
        ) {
            opening = null
        }
    }
}

@Composable
private fun TrayItem(name: String, remove: () -> Unit, content: @Composable () -> Unit) {
    Box {
        Box(Modifier.padding(top = 8.dp, end = 8.dp)) { content() }
        // Small on screen; Compose extends its touch area to the minimum target size.
        Box(
            Modifier.align(Alignment.TopEnd)
                .size(22.dp)
                .clip(CircleShape)
                .background(MaterialTheme.colorScheme.surface)
                .padding(2.dp)
                .clip(CircleShape)
                .background(MaterialTheme.colorScheme.inverseSurface)
                .clickable(onClickLabel = "Retirer") { remove() }
                .semantics { contentDescription = "Retirer $name" },
            contentAlignment = Alignment.Center,
        ) {
            Icon(
                CairnIcons.Close,
                null,
                Modifier.size(11.dp),
                tint = MaterialTheme.colorScheme.inverseOnSurface,
            )
        }
    }
}

// ---- B2/B3: sent messages ----------------------------------------------------------------

/**
 * Files of a sent or queued message: an image mosaic, document cards showing their content, then
 * one-line cards. Fixed sizes keep the history from moving while pictures load.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun MessageAttachments(
    vm: CairnViewModel,
    items: List<ChatAttachment>,
    modifier: Modifier = Modifier,
    alignment: Alignment.Horizontal = Alignment.End,
) {
    val ordered = remember(items) { displayOrder(items) }
    val images = ordered.filter { it.family == AttachmentFamily.IMAGE }
    val documents = ordered.filter {
        it.family != AttachmentFamily.IMAGE && it.hasDocumentPreview()
    }
    val others = ordered.filter { it.family != AttachmentFamily.IMAGE && !it.hasDocumentPreview() }
    var opening by remember { mutableStateOf<ChatAttachment?>(null) }
    var acting by remember { mutableStateOf<ChatAttachment?>(null) }
    fun Modifier.opens(attachment: ChatAttachment) =
        combinedClickable(
            onClickLabel = "Aperçu",
            onLongClickLabel = "Actions",
            onLongClick = { acting = attachment },
        ) {
            opening = attachment
        }
    val arrangement =
        if (alignment == Alignment.End) Arrangement.spacedBy(8.dp, Alignment.End)
        else Arrangement.spacedBy(8.dp)
    Column(
        modifier.testTag("message-attachments"),
        horizontalAlignment = alignment,
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (images.isNotEmpty()) ImageMosaic(vm, images) { Modifier.opens(it) }
        if (documents.isNotEmpty())
            FlowRow(
                horizontalArrangement = arrangement,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                documents.forEach { DocumentCard(vm, it, Modifier.opens(it)) }
            }
        others.forEach { attachment ->
            Box(Modifier.clip(RoundedCornerShape(12.dp)).opens(attachment)) {
                FileCard(vm, attachment, Modifier.size(width = 228.dp, height = 58.dp))
            }
        }
    }
    opening?.let { initial ->
        AttachmentGallery(vm, ordered, initial, actions = { acting = it }) { opening = null }
    }
    AttachmentActions(vm, acting) { acting = null }
}

@Composable
private fun ImageMosaic(
    vm: CairnViewModel,
    images: List<ChatAttachment>,
    opens: (ChatAttachment) -> Modifier,
) {
    val layout = mosaic(images.size)
    val gap = 3.dp
    @Composable
    fun Tile(index: Int, modifier: Modifier) {
        val attachment = images[index]
        Box(modifier.then(opens(attachment)).semantics { contentDescription = attachment.name }) {
            AttachmentPicture(vm, attachment, Modifier.matchParentSize(), target = 240.dp)
            if (index == layout.shown - 1 && layout.hidden > 0)
                Box(
                    Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.45f)),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        "+${layout.hidden}",
                        color = Color.White,
                        style = MaterialTheme.typography.titleLarge,
                        fontWeight = FontWeight.Bold,
                    )
                }
        }
    }
    Box(Modifier.width(240.dp).clip(RoundedCornerShape(16.dp))) {
        when (layout.shown) {
            1 -> Tile(0, Modifier.fillMaxWidth().height(180.dp))
            2 ->
                Row(horizontalArrangement = Arrangement.spacedBy(gap)) {
                    Tile(0, Modifier.weight(1f).height(120.dp))
                    Tile(1, Modifier.weight(1f).height(120.dp))
                }
            3 ->
                Row(
                    Modifier.height(163.dp),
                    horizontalArrangement = Arrangement.spacedBy(gap),
                ) {
                    Tile(0, Modifier.weight(1.4f).fillMaxHeight())
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(gap)) {
                        Tile(1, Modifier.fillMaxWidth().weight(1f))
                        Tile(2, Modifier.fillMaxWidth().weight(1f))
                    }
                }
            else ->
                Column(verticalArrangement = Arrangement.spacedBy(gap)) {
                    for (row in 0..1) Row(horizontalArrangement = Arrangement.spacedBy(gap)) {
                        Tile(row * 2, Modifier.weight(1f).height(118.dp))
                        Tile(row * 2 + 1, Modifier.weight(1f).height(118.dp))
                    }
                }
        }
    }
}

/** A card picturing its content: the first PDF page, or the opening lines of a text file. */
@Composable
private fun DocumentCard(vm: CairnViewModel, attachment: ChatAttachment, modifier: Modifier) {
    val summary = rememberSummary(vm, attachment)
    val pdf = attachment.family == AttachmentFamily.PDF
    Surface(
        Modifier.width(116.dp).clip(RoundedCornerShape(14.dp)).then(modifier),
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column {
            Box(
                Modifier.fillMaxWidth()
                    .height(92.dp)
                    .background(MaterialTheme.colorScheme.surfaceVariant)
                    .clipToBounds()
            ) {
                if (pdf)
                    AttachmentPicture(
                        vm,
                        attachment,
                        Modifier.padding(start = 14.dp, end = 14.dp, top = 10.dp)
                            .fillMaxSize()
                            .background(Color.White),
                        target = 160.dp,
                        alignment = Alignment.TopCenter,
                    )
                else
                    Text(
                        summary?.excerpt.orEmpty(),
                        Modifier.padding(8.dp),
                        fontFamily = FontFamily.Monospace,
                        fontSize = 7.sp,
                        lineHeight = 9.sp,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        overflow = TextOverflow.Clip,
                    )
                FileBadge(
                    attachment.name,
                    attachment.family,
                    Modifier.align(Alignment.BottomStart)
                        .padding(6.dp)
                        .size(width = 30.dp, height = 16.dp),
                )
            }
            Column(Modifier.padding(start = 8.dp, end = 8.dp, top = 6.dp, bottom = 8.dp)) {
                AttachmentName(attachment.name, MaterialTheme.typography.labelMedium)
                Text(
                    attachmentDetail(attachment, summary) ?: fileSize(attachment.size.toLong()),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

// ---- C: viewer ---------------------------------------------------------------------------

/** Downloads (or reads locally) one attachment for the viewer and the actions. */
private suspend fun attachmentFile(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    localPath: String?,
): File = localPath?.let { File(it) } ?: fetchOriginal(vm.files, vm.api, attachment)

private suspend fun fetchOriginal(files: Files, api: CairnApi, attachment: ChatAttachment): File {
    val mime = attachmentMediaType(attachment)
    val imageExtension =
        if (mime.startsWith("image/")) MimeTypeMap.getSingleton().getExtensionFromMimeType(mime)
        else null
    // FileProvider derives its MIME from the physical filename, including for the clipboard.
    // Keep a typed cache name even for renamed images; actions expose the original display name.
    val cacheName = imageExtension?.let { "${attachment.name}.$it" } ?: attachment.name
    return files.fetch(
        api,
        "${attachmentPath(attachment)}/stream",
        cacheName,
        MAX_ATTACHMENT_BYTES,
    )
}

private fun shareFile(context: Context, file: File, attachment: ChatAttachment) {
    val uri =
        FileProvider.getUriForFile(context, "${context.packageName}.files", file, attachment.name)
    val intent =
        Intent(Intent.ACTION_SEND)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            .setType(attachmentMediaType(attachment))
            .putExtra(Intent.EXTRA_STREAM, uri)
            .putExtra(Intent.EXTRA_TITLE, attachment.name)
    intent.clipData = ClipData.newRawUri(attachment.name, uri)
    context.startActivity(Intent.createChooser(intent, attachment.name))
}

private fun copyImage(context: Context, file: File, attachment: ChatAttachment) {
    val uri =
        FileProvider.getUriForFile(context, "${context.packageName}.files", file, attachment.name)
    context
        .getSystemService(ClipboardManager::class.java)
        .setPrimaryClip(ClipData.newUri(context.contentResolver, attachment.name, uri))
}

/** Opens the system « Enregistrer sous » with the file's name and type. */
private object SaveDocument : ActivityResultContract<ChatAttachment, Uri?>() {
    override fun createIntent(context: Context, input: ChatAttachment) =
        Intent(Intent.ACTION_CREATE_DOCUMENT)
            .addCategory(Intent.CATEGORY_OPENABLE)
            .setType(attachmentMediaType(input))
            .putExtra(Intent.EXTRA_TITLE, input.name)

    override fun parseResult(resultCode: Int, intent: Intent?) =
        intent?.data.takeIf { resultCode == android.app.Activity.RESULT_OK }
}

/** Saves through the system picker, downloading the file once a destination is chosen. */
@Composable
private fun rememberSave(
    vm: CairnViewModel,
    local: Map<String, String> = emptyMap(),
): (ChatAttachment) -> Unit {
    var saving by rememberSaveable { mutableStateOf<String?>(null) }
    val launcher =
        rememberLauncherForActivityResult(SaveDocument) { uri ->
            val attachment = saving?.let { wireJson.decodeFromString<ChatAttachment>(it) }
            saving = null
            if (uri != null && attachment != null)
                vm.viewModelScope.launch {
                    try {
                        vm.files.save(attachmentFile(vm, attachment, local[attachment.id]), uri)
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        vm.report(e)
                    }
                }
        }
    return { attachment ->
        saving = wireJson.encodeToString(attachment)
        launcher.launch(attachment)
    }
}

/** Runs an action that needs the file, reporting download failures. */
@Composable
private fun rememberFileAction(
    vm: CairnViewModel,
    local: Map<String, String> = emptyMap(),
): (ChatAttachment, (File) -> Unit) -> Unit {
    val scope = rememberCoroutineScope()
    return { attachment, action ->
        scope.launch {
            try {
                action(attachmentFile(vm, attachment, local[attachment.id]))
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                vm.report(e)
            }
        }
    }
}

/**
 * Full-screen pages through the files of one message (or of the draft), with a filmstrip. Images
 * zoom; when not zoomed, a downward swipe closes.
 */
@Composable
internal fun AttachmentGallery(
    vm: CairnViewModel,
    items: List<ChatAttachment>,
    initial: ChatAttachment,
    local: Map<String, String> = emptyMap(),
    actions: ((ChatAttachment) -> Unit)? = null,
    close: () -> Unit,
) {
    val workspace by vm.state.collectAsStateWithLifecycle()
    val pages = remember { items.ifEmpty { listOf(initial) } }
    val pager =
        rememberPagerState(
            initialPage = pages.indexOfFirst { it.id == initial.id }.coerceAtLeast(0)
        ) {
            pages.size
        }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val save = rememberSave(vm, local)
    val withFile = rememberFileAction(vm, local)
    var zoomedId by remember { mutableStateOf<String?>(null) }
    var drag by remember { mutableFloatStateOf(0f) }
    val dismissDistance = with(LocalDensity.current) { 140.dp.toPx() }
    val current = pages[pager.currentPage]
    val strip = rememberLazyListState()
    LaunchedEffect(pager.currentPage) { strip.animateScrollToItem(pager.currentPage) }
    Dialog(
        onDismissRequest = close,
        // Edge to edge: the dark viewer also covers the status and navigation bars.
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        CairnTheme("dark") {
            Surface(
                Modifier.fillMaxSize().testTag("attachment-viewer"),
                color = MaterialTheme.colorScheme.background,
            ) {
                Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        ActionIcon("Fermer", CairnIcons.Close, onClick = close)
                        Column(Modifier.weight(1f).padding(horizontal = 4.dp)) {
                            AttachmentName(current.name, MaterialTheme.typography.titleSmall)
                            Text(
                                attachmentSubtitle(current) +
                                    if (pages.size > 1)
                                        " · ${pager.currentPage + 1} / ${pages.size}"
                                    else "",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                            )
                        }
                        ActionIcon("Enregistrer", CairnIcons.Download) { save(current) }
                        ActionIcon("Partager", Icons.Default.Share) {
                            withFile(current) { shareFile(context, it, current) }
                        }
                        if (actions != null)
                            ActionIcon("Plus d’actions", CairnIcons.More) { actions(current) }
                    }
                    ConnectionFeedbackNotice()
                    workspace.error?.let { ErrorNotice(it, vm::clearMessage) }
                    HorizontalPager(
                        state = pager,
                        modifier =
                            Modifier.weight(1f)
                                .fillMaxWidth()
                                .draggable(
                                    rememberDraggableState {
                                        drag = (drag + it).coerceAtLeast(0f)
                                    },
                                    Orientation.Vertical,
                                    enabled =
                                        current.family == AttachmentFamily.IMAGE &&
                                            zoomedId != current.id,
                                    onDragStopped = {
                                        if (drag > dismissDistance) close() else drag = 0f
                                    },
                                )
                                .graphicsLayer {
                                    translationY = drag
                                    alpha = 1f - (drag / (dismissDistance * 3)).coerceIn(0f, 0.6f)
                                }
                                .testTag("attachment-pager"),
                        key = { pages[it].id },
                        beyondViewportPageCount = 1,
                        userScrollEnabled = pages.size > 1 && zoomedId != current.id,
                    ) { page ->
                        val item = pages[page]
                        AttachmentPage(
                            vm,
                            item,
                            local[item.id],
                            active = page == pager.settledPage,
                        ) { zoomed ->
                            if (zoomed) zoomedId = item.id
                            else if (zoomedId == item.id) zoomedId = null
                        }
                    }
                    if (pages.size > 1)
                        LazyRow(
                            Modifier.fillMaxWidth().padding(vertical = 10.dp),
                            state = strip,
                            contentPadding = PaddingValues(horizontal = 16.dp),
                            horizontalArrangement =
                                Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
                        ) {
                            itemsIndexed(pages, key = { _, item -> item.id }) { index, item ->
                                val selected = index == pager.currentPage
                                Box(
                                    Modifier.size(44.dp)
                                        .alpha(if (selected) 1f else 0.55f)
                                        .border(
                                            2.dp,
                                            if (selected) Color.White else Color.Transparent,
                                            RoundedCornerShape(9.dp),
                                        )
                                        .padding(3.dp)
                                        .clip(RoundedCornerShape(7.dp))
                                        .clickable(onClickLabel = item.name) {
                                            scope.launch { pager.animateScrollToPage(index) }
                                        }
                                ) {
                                    if (item.family == AttachmentFamily.IMAGE)
                                        AttachmentPicture(
                                            vm,
                                            item,
                                            Modifier.fillMaxSize(),
                                            local[item.id],
                                        )
                                    else FileBadge(item.name, item.family, Modifier.fillMaxSize())
                                }
                            }
                        }
                }
            }
        }
    }
}

@Composable
private fun AttachmentPage(
    vm: CairnViewModel,
    attachment: ChatAttachment,
    localPath: String?,
    active: Boolean,
    zoomChanged: (Boolean) -> Unit,
) {
    val (kind, mime) = remember(attachment) { viewerType(attachment) }
    var file by remember(attachment.id, localPath) { mutableStateOf<File?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var retry by remember { mutableIntStateOf(0) }
    // Neighbours preload, except media which only plays on its own page.
    val load = active || kind !in setOf("audio", "video")
    LaunchedEffect(attachment.id, localPath, retry, load) {
        if (!load || file != null) return@LaunchedEffect
        error = null
        try {
            file = attachmentFile(vm, attachment, localPath)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "Téléchargement impossible."
        }
    }
    Column(Modifier.fillMaxSize()) {
        val current = file
        when {
            current != null ->
                FileContent(current, attachment.name, mime, kind, active, zoomChanged)
            error != null ->
                Column(Modifier.padding(20.dp)) {
                    Text(error!!, color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { retry++ }) { Text("Réessayer") }
                }
            else ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator()
                }
        }
    }
}

// ---- C': actions -------------------------------------------------------------------------

/** Long press on a sent file: save, share, copy an image, or attach it again. */
@Composable
internal fun AttachmentActions(vm: CairnViewModel, attachment: ChatAttachment?, close: () -> Unit) {
    val context = LocalContext.current
    val save = rememberSave(vm)
    val withFile = rememberFileAction(vm)
    // The launcher and download scope outlive dismissal of the sheet.
    if (attachment == null) return

    val reuse = LocalAttachmentReuse.current?.takeIf { it.chatId == attachment.chatId }
    ModalBottomSheet(onDismissRequest = close) {
        Column(Modifier.fillMaxWidth().padding(bottom = 24.dp).testTag("attachment-actions")) {
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 8.dp),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (attachment.family == AttachmentFamily.IMAGE)
                    AttachmentPicture(
                        vm,
                        attachment,
                        Modifier.size(44.dp).clip(RoundedCornerShape(8.dp)),
                    )
                else FileBadge(attachment.name, attachment.family, Modifier.size(38.dp, 44.dp))
                Column(Modifier.weight(1f)) {
                    Text(attachment.name, style = MaterialTheme.typography.titleSmall)
                    Text(
                        attachmentSubtitle(attachment),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            HorizontalDivider(Modifier.padding(vertical = 4.dp))
            fun then(action: () -> Unit): () -> Unit = {
                close()
                action()
            }
            AttachmentAction(CairnIcons.Download, "Enregistrer", then { save(attachment) })
            AttachmentAction(
                Icons.Default.Share,
                "Partager",
                then { withFile(attachment) { shareFile(context, it, attachment) } },
            )
            if (attachment.family == AttachmentFamily.IMAGE)
                AttachmentAction(
                    CairnIcons.Copy,
                    "Copier l’image",
                    then { withFile(attachment) { copyImage(context, it, attachment) } },
                )
            if (reuse != null)
                AttachmentAction(
                    CairnIcons.Retry,
                    "Réutiliser dans un nouveau message",
                    then { reuse.add(attachment) },
                )
        }
    }
}

@Composable
internal fun AttachmentAction(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    onClick: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 56.dp)
            .clickable(onClick = onClick)
            .padding(horizontal = 24.dp),
        horizontalArrangement = Arrangement.spacedBy(16.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, null, Modifier.size(22.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge)
    }
}
