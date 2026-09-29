@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package dev.leo.manager.ui

import android.content.ClipData
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.pdf.PdfRenderer
import android.os.ParcelFileDescriptor
import android.widget.MediaController
import android.widget.VideoView
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import androidx.core.graphics.createBitmap
import dev.leo.manager.data.*
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
fun ArtifactsPanel(vm: LeoViewModel, artifacts: List<Deliverable>) {
    var allVersions by rememberSaveable { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var opening by remember { mutableStateOf<Deliverable?>(null) }
    val shown =
        (if (allVersions) artifacts else latestArtifacts(artifacts)).filter {
            "${it.title} ${it.name} ${it.group}".contains(query, true)
        }
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        if (artifacts.isEmpty()) Text("Les fichiers publiés par l’agent apparaîtront ici.")
        else {
            SearchField("Rechercher un fichier", query, { query = it })
            Toggle("Afficher toutes les versions", allVersions) { allVersions = it }
            shown
                .groupBy { it.group }
                .forEach { (group, entries) ->
                    if (group.isNotBlank())
                        Text(group, style = MaterialTheme.typography.titleMedium)
                    BoxWithConstraints(Modifier.fillMaxWidth()) {
                        val columns =
                            if (maxWidth >= 540.dp) 3 else if (maxWidth >= 300.dp) 2 else 1
                        Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                            entries.chunked(columns).forEach { row ->
                                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                                    row.forEach { artifact ->
                                        Box(Modifier.weight(1f)) {
                                            ArtifactTile(vm, artifact) { opening = artifact }
                                        }
                                    }
                                    repeat(columns - row.size) { Spacer(Modifier.weight(1f)) }
                                }
                            }
                        }
                    }
                }
        }
    }
    opening?.let { item ->
        ArtifactGallery(vm, shown.groupBy { it.group }.values.flatten(), item) { opening = null }
    }
}

@Composable
internal fun ArtifactStrip(vm: LeoViewModel, artifacts: List<Deliverable>) {
    val openArtifact = LocalArtifactLinks.current
    var opening by remember { mutableStateOf<Deliverable?>(null) }
    // The transcript is a reader, not a file gallery. Keep every file accessible
    // in one compact rail; full previews, groups and versions remain in Files.
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        val rowWidth = minOf(maxWidth, 280.dp)
        LazyRow(
            Modifier.testTag("conversation-files"),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            items(artifacts, key = { it.id }) { artifact ->
                Surface(
                    onClick = {
                        if (!openArtifact("/api" + artifact.path())) opening = artifact
                    },
                    modifier = Modifier.width(rowWidth),
                    color = MaterialTheme.colorScheme.background,
                    shape = RoundedCornerShape(8.dp),
                ) {
                    Column {
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        Row(
                            Modifier.fillMaxWidth().heightIn(min = 72.dp).padding(vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            Box(
                                Modifier.size(44.dp)
                                    .clip(RoundedCornerShape(6.dp))
                                    .background(MaterialTheme.colorScheme.surface),
                                contentAlignment = Alignment.Center,
                            ) {
                                Icon(
                                    LeoIcons.File,
                                    null,
                                    Modifier.size(22.dp),
                                    tint = MaterialTheme.colorScheme.primary,
                                )
                                if (artifact.previewStatus == "ready")
                                    ArtifactThumbnail(vm, artifact)
                            }
                            Column(
                                Modifier.weight(1f),
                                verticalArrangement = Arrangement.spacedBy(3.dp),
                            ) {
                                Text(
                                    artifact.title.ifBlank { artifact.name },
                                    style = MaterialTheme.typography.titleSmall,
                                    maxLines = 2,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                Text(
                                    "${artifact.name.substringAfterLast('.', artifact.kind).uppercase()} · ${fileSize(artifact.size)}" +
                                        if (artifact.version > 1) " · v${artifact.version}" else "",
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                            Icon(
                                LeoIcons.Right,
                                null,
                                Modifier.size(16.dp),
                                tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    }
                }
            }
        }
    }
    opening?.let { artifact ->
        ArtifactGallery(vm, artifacts, artifact) { opening = null }
    }
}

@Composable
private fun ArtifactTile(vm: LeoViewModel, artifact: Deliverable, open: () -> Unit) {
    Card(
        onClick = open,
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant.copy(alpha = 0.65f)),
        colors =
            CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        Box(
            Modifier.fillMaxWidth()
                .height(104.dp)
                .background(MaterialTheme.colorScheme.surfaceVariant),
            contentAlignment = Alignment.Center,
        ) {
            if (artifact.previewStatus == "ready") ArtifactThumbnail(vm, artifact)
            else if (!artifact.excerpt.isNullOrBlank())
                Text(
                    artifact.excerpt.replace("**", "").replace("#", ""),
                    Modifier.padding(14.dp),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 5,
                    overflow = TextOverflow.Ellipsis,
                )
            else
                Icon(
                    if (artifact.kind in listOf("audio", "video")) Icons.Default.PlayArrow
                    else LeoIcons.File,
                    null,
                    Modifier.size(32.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
            if (artifact.version > 1)
                Surface(
                    Modifier.align(Alignment.TopEnd).padding(6.dp),
                    shape = MaterialTheme.shapes.small,
                ) {
                    Text(
                        "v${artifact.version}",
                        Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
                        style = MaterialTheme.typography.labelSmall,
                    )
                }
        }
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                artifact.title.ifBlank { artifact.name },
                style = MaterialTheme.typography.titleSmall,
                maxLines = 2,
                minLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                "${artifact.name.substringAfterLast('.', artifact.kind).uppercase()} · ${fileSize(artifact.size)}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

@Composable
fun AttachmentList(
    vm: LeoViewModel,
    items: List<ChatAttachment>,
    remove: ((String) -> Unit)? = null,
    local: Map<String, String> = emptyMap(),
) {
    var opening by remember { mutableStateOf<ChatAttachment?>(null) }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        items.forEach { item ->
            Row(Modifier.fillMaxWidth()) {
                TextButton(onClick = { opening = item }, modifier = Modifier.weight(1f)) {
                    Text("${item.name} · ${fileSize(item.size)}", maxLines = 2)
                }
                if (remove != null)
                    ActionIcon("Retirer ${item.name}", Icons.Default.Close) { remove(item.id) }
            }
        }
    }
    opening?.let { item ->
        FilePreview(
            vm,
            "/chats/${segment(item.chatId)}/attachments/${segment(item.id)}",
            item.name,
            item.mediaType,
            item.kind,
            local[item.id],
        ) {
            opening = null
        }
    }
}

/** Freeze the browsing order for this opening, even if the conversation publishes new files. */
@Composable
internal fun ArtifactGallery(
    vm: LeoViewModel,
    artifacts: List<Deliverable>,
    initial: Deliverable,
    close: () -> Unit,
) {
    key(initial.id) {
        val pages = remember {
            (if (artifacts.any { it.id == initial.id }) artifacts else artifacts + initial)
                .distinctBy { it.id }
        }
        val pager =
            rememberPagerState(initialPage = pages.indexOfFirst { it.id == initial.id }) {
                pages.size
            }
        val scope = rememberCoroutineScope()
        var zoomedId by remember { mutableStateOf<String?>(null) }
        Dialog(
            onDismissRequest = close,
            properties = DialogProperties(usePlatformDefaultWidth = false),
        ) {
            HorizontalPager(
                state = pager,
                modifier =
                    Modifier.fillMaxSize()
                        .background(MaterialTheme.colorScheme.background)
                        .testTag("artifact-pager"),
                key = { pages[it].id },
                beyondViewportPageCount = 1,
                userScrollEnabled = pages.size > 1 && zoomedId != pages[pager.settledPage].id,
            ) { page ->
                val item = pages[page]
                val active = page == pager.settledPage
                Box(
                    Modifier.fillMaxSize()
                        .clipToBounds()
                        .testTag("artifact-page:${item.id}")
                        .semantics { selected = active }
                ) {
                    Box(
                        Modifier.fillMaxSize()
                            .focusProperties { canFocus = active }
                            .then(if (active) Modifier else Modifier.clearAndSetSemantics {})
                    ) {
                        FilePreviewPage(
                            vm,
                            item.path(),
                            item.name,
                            item.mediaType,
                            item.kind,
                            version = item.version,
                            artifact = item,
                            active = active,
                            previous =
                                if (page > 0)
                                    ({
                                        scope.launch { pager.animateScrollToPage(page - 1) }
                                        Unit
                                    })
                                else null,
                            next =
                                if (page < pages.lastIndex)
                                    ({
                                        scope.launch { pager.animateScrollToPage(page + 1) }
                                        Unit
                                    })
                                else null,
                            position = "${page + 1} / ${pages.size}",
                            zoomChanged = { zoomed ->
                                if (zoomed) zoomedId = item.id
                                else if (zoomedId == item.id) zoomedId = null
                            },
                            close = close,
                        )
                    }
                }
            }
        }
    }
}

@Composable
internal fun FilePreview(
    vm: LeoViewModel,
    path: String,
    name: String,
    mime: String,
    kind: String,
    localPath: String? = null,
    close: () -> Unit,
) {
    Dialog(
        onDismissRequest = close,
        properties = DialogProperties(usePlatformDefaultWidth = false),
    ) {
        FilePreviewPage(vm, path, name, mime, kind, localPath, close = close)
    }
}

@Composable
private fun FilePreviewPage(
    vm: LeoViewModel,
    path: String,
    name: String,
    mime: String,
    kind: String,
    localPath: String? = null,
    previous: (() -> Unit)? = null,
    next: (() -> Unit)? = null,
    version: Int? = null,
    artifact: Deliverable? = null,
    active: Boolean = true,
    position: String? = null,
    zoomChanged: (Boolean) -> Unit = {},
    close: () -> Unit,
) {
    var sharing by remember { mutableStateOf(false) }
    if (sharing && artifact != null) ArtifactSharingDialog(vm, artifact) { sharing = false }
    var file by remember(path, localPath) { mutableStateOf<File?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var retry by remember { mutableIntStateOf(0) }
    val context = LocalContext.current
    val save =
        rememberLauncherForActivityResult(
            ActivityResultContracts.CreateDocument(mime.ifBlank { "application/octet-stream" })
        ) { uri ->
            if (uri != null && file != null) vm.perform { files.save(file!!, uri) }
        }
    val load =
        active ||
            (artifact != null &&
                artifact.size in 1..10L * 1024 * 1024 &&
                kind !in listOf("video", "audio") &&
                !mime.startsWith("video/") &&
                !mime.startsWith("audio/"))
    LaunchedEffect(path, localPath, retry, load) {
        if (!load) return@LaunchedEffect
        error = null
        if (file != null) return@LaunchedEffect
        try {
            file = localPath?.let { File(it) } ?: vm.files.fetch(vm.api, path, name)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message
        }
    }
    fun launch(action: String) {
        val current = file ?: return
        try {
            val uri =
                FileProvider.getUriForFile(context, "${context.packageName}.files", current, name)
            val intent = Intent(action).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            intent.clipData = ClipData.newRawUri(name, uri)
            if (action == Intent.ACTION_SEND) {
                intent.type = mime
                intent.putExtra(Intent.EXTRA_STREAM, uri)
                intent.putExtra(Intent.EXTRA_TITLE, name)
            } else intent.setDataAndType(uri, mime)
            context.startActivity(Intent.createChooser(intent, name))
        } catch (e: Exception) {
            error = "Aucune application ne peut ouvrir ce fichier : ${e.message}"
        }
    }
    // Read once at this scope so the toolbar and body receive the same loading state,
    // including when the pager precomposes a page before its download finishes.
    val currentFile = file
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Column {
                        Text(
                            name,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.titleMedium,
                        )
                        if (version != null)
                            Text(
                                "Version $version",
                                style = MaterialTheme.typography.labelSmall,
                            )
                    }
                },
                actions = {
                    if (artifact != null)
                        ActionIcon("Lien public", Icons.Default.Share) { sharing = true }
                    ActionIcon("Enregistrer", LeoIcons.Download, currentFile != null) {
                        save.launch(name)
                    }
                    Box {
                        var menu by remember { mutableStateOf(false) }
                        ActionIcon("Options du fichier", Icons.Default.MoreVert) { menu = true }
                        DropdownMenu(menu, { menu = false }) {
                            DropdownMenuItem(
                                text = { Text("Partager") },
                                enabled = currentFile != null,
                                onClick = {
                                    menu = false
                                    launch(Intent.ACTION_SEND)
                                },
                            )
                            DropdownMenuItem(
                                text = { Text("Ouvrir avec") },
                                enabled = currentFile != null,
                                onClick = {
                                    menu = false
                                    launch(Intent.ACTION_VIEW)
                                },
                            )
                        }
                    }
                },
                navigationIcon = {
                    IconButton(onClick = close) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer le fichier")
                    }
                },
            )
        }
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            if (previous != null || next != null)
                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    ActionIcon(
                        "Précédent",
                        Icons.AutoMirrored.Filled.ArrowBack,
                        previous != null,
                    ) {
                        previous?.invoke()
                    }
                    Text(
                        position ?: "Parcourir les fichiers",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    ActionIcon("Suivant", LeoIcons.Right, next != null) { next?.invoke() }
                }
            error?.let { message ->
                Text(message, Modifier.padding(16.dp), color = MaterialTheme.colorScheme.error)
                TextButton(onClick = { retry++ }) { Text("Réessayer") }
            }
            currentFile?.let { current ->
                when {
                    kind == "image" || mime.startsWith("image/") ->
                        NativeImage(current, name, active, zoomChanged)
                    kind == "pdf" || mime == "application/pdf" -> NativePdf(current)
                    kind in listOf("video", "audio") ||
                        mime.startsWith("video/") ||
                        mime.startsWith("audio/") -> NativeMedia(current, active)
                    kind in listOf("markdown", "code") ||
                        mime.startsWith("text/") ||
                        mime == "application/json" -> {
                        var text by remember(current) { mutableStateOf("") }
                        LaunchedEffect(current) {
                            text =
                                withContext(Dispatchers.IO) {
                                    current.inputStream().bufferedReader().use { reader ->
                                        val chars = CharArray(1024 * 1024)
                                        val count = reader.read(chars)
                                        if (count < 0) "" else String(chars, 0, count)
                                    }
                                }
                        }
                        Page {
                            if (kind == "markdown" || mime.contains("markdown")) Markdown(text)
                            else if (mime == "application/json" || name.endsWith(".json", true)) {
                                if (text.length <= 500_000) ResultContent(text)
                                else
                                    Text(
                                        "Fichier volumineux : consultez la source ou enregistrez-le pour lire la suite."
                                    )
                                Disclosure("Voir la source JSON") { Code(text) }
                            } else Code(text)
                            if (current.length() > 1024 * 1024)
                                Text(
                                    "Aperçu limité à 1 Mo. Enregistrez le fichier pour lire la suite."
                                )
                        }
                    }
                    else ->
                        Page {
                            Text("${fileSize(current.length())} · $mime")
                            Text(
                                "Enregistrez ce fichier ou ouvrez-le dans une application compatible."
                            )
                        }
                }
            }
                ?: if (error == null)
                    Box(
                        Modifier.fillMaxSize(),
                        contentAlignment = androidx.compose.ui.Alignment.Center,
                    ) {
                        if (load) CircularProgressIndicator()
                        else
                            Text(
                                "Aperçu au relâchement",
                                style = MaterialTheme.typography.labelMedium,
                            )
                    }
                else Unit
        }
    }
}

@Composable
private fun NativeImage(
    file: File,
    description: String,
    active: Boolean,
    zoomChanged: (Boolean) -> Unit,
) {
    var bitmap by remember(file) { mutableStateOf<Bitmap?>(null) }
    var failed by remember(file) { mutableStateOf(false) }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(androidx.compose.ui.geometry.Offset.Zero) }
    LaunchedEffect(file) {
        bitmap =
            withContext(Dispatchers.IO) {
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeFile(file.path, bounds)
                val sample =
                    Integer.highestOneBit(
                        (maxOf(bounds.outWidth, bounds.outHeight) / 2048).coerceAtLeast(1)
                    )
                BitmapFactory.decodeFile(
                    file.path,
                    BitmapFactory.Options().apply { inSampleSize = sample },
                )
            }
        failed = bitmap == null
    }
    val transform = rememberTransformableState { zoom, pan, _ ->
        scale = (scale * zoom).coerceIn(1f, 6f)
        offset = if (scale == 1f) androidx.compose.ui.geometry.Offset.Zero else offset + pan
    }
    LaunchedEffect(active, scale) { if (active) zoomChanged(scale > 1f) }
    LaunchedEffect(active) {
        if (!active) {
            scale = 1f
            offset = androidx.compose.ui.geometry.Offset.Zero
        }
    }
    DisposableEffect(Unit) { onDispose { zoomChanged(false) } }
    bitmap?.let {
        Image(
            it.asImageBitmap(),
            description,
            Modifier.fillMaxSize().transformable(transform, canPan = { scale > 1f }).graphicsLayer {
                scaleX = scale
                scaleY = scale
                translationX = offset.x
                translationY = offset.y
            },
        )
    }
    if (failed)
        Text(
            "Aperçu indisponible. Vous pouvez ouvrir l’image dans une autre application.",
            Modifier.padding(20.dp),
        )
}

@Composable
private fun NativePdf(file: File) {
    var page by rememberSaveable { mutableIntStateOf(0) }
    var total by remember { mutableIntStateOf(0) }
    var bitmap by remember { mutableStateOf<Bitmap?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(file, page) {
        try {
            val result =
                withContext(Dispatchers.IO) {
                    ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY).use {
                        descriptor ->
                        PdfRenderer(descriptor).use { pdf ->
                            pdf.openPage(page.coerceIn(0, pdf.pageCount - 1)).use { content ->
                                val width = 1200
                                val image =
                                    createBitmap(
                                        width,
                                        (width.toLong() * content.height / content.width)
                                            .coerceIn(1, 4000)
                                            .toInt(),
                                        Bitmap.Config.ARGB_8888,
                                    )
                                image.eraseColor(android.graphics.Color.WHITE)
                                content.render(
                                    image,
                                    null,
                                    null,
                                    PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY,
                                )
                                pdf.pageCount to image
                            }
                        }
                    }
                }
            total = result.first
            bitmap = result.second
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "Fichier invalide"
        }
    }
    Column {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
            TextButton(onClick = { page-- }, enabled = page > 0) { Text("Précédente") }
            Text("${page + 1} / $total", Modifier.padding(12.dp))
            TextButton(onClick = { page++ }, enabled = page + 1 < total) { Text("Suivante") }
        }
        bitmap?.let { Image(it.asImageBitmap(), "Page ${page + 1}", Modifier.fillMaxSize()) }
        error?.let { Text("Aperçu PDF indisponible : $it", Modifier.padding(16.dp)) }
    }
}

@Composable
private fun NativeMedia(file: File, active: Boolean) {
    var video by remember { mutableStateOf<VideoView?>(null) }
    if (!active) {
        Box(Modifier.fillMaxWidth().height(320.dp), contentAlignment = Alignment.Center) {
            Icon(Icons.Default.PlayArrow, "Aperçu multimédia", Modifier.size(48.dp))
        }
        return
    }
    AndroidView(
        factory = { context ->
            VideoView(context).apply {
                video = this
                setVideoPath(file.path)
                val player = this
                setMediaController(MediaController(context).apply { setAnchorView(player) })
                setOnPreparedListener { seekTo(1) }
            }
        },
        modifier = Modifier.fillMaxWidth().height(320.dp),
    )
    TextButton(onClick = { video?.start() }) { Text("Lire") }
    DisposableEffect(file) { onDispose { video?.stopPlayback() } }
}

@Composable
private fun ArtifactThumbnail(vm: LeoViewModel, artifact: Deliverable) {
    var bitmap by remember(artifact.id) { mutableStateOf<Bitmap?>(null) }
    LaunchedEffect(artifact.id) {
        try {
            val file =
                vm.files.fetch(
                    vm.api,
                    artifact.path(preview = true),
                    "preview.png",
                    10L * 1024 * 1024,
                )
            bitmap =
                withContext(Dispatchers.IO) {
                    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                    BitmapFactory.decodeFile(file.path, bounds)
                    BitmapFactory.decodeFile(
                        file.path,
                        BitmapFactory.Options().apply {
                            inSampleSize =
                                Integer.highestOneBit(
                                    (maxOf(bounds.outWidth, bounds.outHeight) / 512).coerceAtLeast(
                                        1
                                    )
                                )
                        },
                    )
                }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Exception) {
            /* The file remains accessible when a preview is unavailable. */
        }
    }
    bitmap?.let {
        Image(it.asImageBitmap(), artifact.title, Modifier.fillMaxSize())
    }
}

internal val LocalArtifactLinks = staticCompositionLocalOf<(String) -> Boolean> { { false } }

@Composable
internal fun ArtifactLinkHost(
    vm: LeoViewModel,
    artifacts: List<Deliverable>,
    content: @Composable () -> Unit,
) {
    var opening by remember { mutableStateOf<Deliverable?>(null) }
    var openingFiles by remember { mutableStateOf(emptyList<Deliverable>()) }
    var pending by remember { mutableStateOf<String?>(null) }
    var failure by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(pending) {
        val path = pending ?: return@LaunchedEffect
        try {
            val files = vm.api.get<List<Deliverable>>(path.substringBeforeLast('/'))
            openingFiles = latestArtifacts(files)
            opening =
                files.find { it.path().substringBefore('?') == path }
                    ?: error("Ce fichier n’est plus disponible.")
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            failure = e.message ?: "Impossible d’ouvrir ce fichier."
        } finally {
            if (pending == path) pending = null
        }
    }
    CompositionLocalProvider(
        LocalArtifactLinks provides
            { link ->
                val item = artifactForLink(link, vm.api.origin, artifacts)
                val path = artifactPathForLink(link, vm.api.origin)
                if (path == null) false
                else {
                    failure = null
                    if (item != null) {
                        pending = null
                        openingFiles = latestArtifacts(artifacts)
                        opening = item
                    } else {
                        opening = null
                        pending = path
                    }
                    true
                }
            }
    ) {
        content()
        if (pending != null || failure != null) {
            AlertDialog(
                onDismissRequest = {
                    pending = null
                    failure = null
                },
                title = {
                    Text(if (failure == null) "Ouverture du fichier…" else "Fichier indisponible")
                },
                text = {
                    if (failure != null) Text(failure!!)
                    else LinearProgressIndicator(Modifier.fillMaxWidth())
                },
                confirmButton = {
                    TextButton(
                        onClick = {
                            pending = null
                            failure = null
                        }
                    ) {
                        Text("Fermer")
                    }
                },
            )
        }
        opening?.let { item ->
            ArtifactGallery(vm, openingFiles, item) { opening = null }
        }
    }
}
