package build.cairn.app.ui

import android.util.Base64
import android.view.ViewGroup
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.foundation.gestures.rememberTransformableState
import androidx.compose.foundation.gestures.transformable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

internal data class TablePreview(
    val rows: List<List<String>>,
    val columns: Int,
    val truncated: Boolean,
)

/** Parses RFC 4180 style CSV/TSV, bounding the rendered table to 200 rows and 50 columns. */
internal fun parseDelimited(source: String, delimiter: Char, limit: Int = 200): TablePreview {
    val rows = mutableListOf<List<String>>()
    var row = mutableListOf<String>()
    val cell = StringBuilder()
    var quoted = false
    var rowStarted = false
    var rowsTruncated = false
    var columnsTruncated = false
    val rowLimit = limit.coerceIn(1, 200)

    fun pushCell() {
        if (row.size < 50) row.add(cell.toString()) else columnsTruncated = true
        cell.clear()
    }
    val text = source.removePrefix(Char(0xFEFF).toString())
    var index = 0

    while (index < text.length) {
        val char = text[index]
        if (quoted || (char != '\n' && char != '\r')) rowStarted = true
        if (quoted) {
            if (char == '"' && text.getOrNull(index + 1) == '"') {
                if (row.size < 50) cell.append('"')
                index++
            } else if (char == '"') quoted = false else if (row.size < 50) cell.append(char)
        } else if (char == '"' && cell.isEmpty()) {
            quoted = true
        } else if (char == delimiter) {
            pushCell()
        } else if (char == '\n' || char == '\r') {
            if (char == '\r' && text.getOrNull(index + 1) == '\n') index++
            pushCell()
            rows.add(row)
            rowStarted = false
            row = mutableListOf()
            cell.clear()
            if (rows.size >= rowLimit) {
                rowsTruncated = index + 1 < text.length
                break
            }
        } else if (row.size < 50) cell.append(char)
        index++
    }

    if (!rowsTruncated && (rowStarted || cell.isNotEmpty() || row.isNotEmpty())) {
        pushCell()
        rows.add(row)
    }

    return TablePreview(rows, rows.maxOfOrNull { it.size } ?: 0, rowsTruncated || columnsTruncated)
}

@Composable
internal fun DelimitedTable(table: TablePreview) {
    Column(Modifier.horizontalScroll(rememberScrollState())) {
        table.rows.forEachIndexed { index, row ->
            Row {
                repeat(table.columns) { column ->
                    SelectionContainer {
                        Text(
                            row.getOrElse(column) { "" },
                            Modifier.width(160.dp).padding(horizontal = 8.dp, vertical = 6.dp),
                            style = MaterialTheme.typography.bodySmall,
                            fontWeight = if (index == 0) FontWeight.SemiBold else null,
                        )
                    }
                }
            }
            HorizontalDivider(
                Modifier.width(160.dp * table.columns),
                color = MaterialTheme.colorScheme.outlineVariant,
            )
        }
    }
    if (table.truncated)
        Text(
            "Aperçu limité à ${table.rows.size} lignes et ${table.columns} colonnes. Enregistrez le fichier pour tout consulter.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
}

/** Displays SVG or animated GIF bytes as an image, without interpreting them as HTML. */
internal fun imageDocument(bytes: ByteArray, mediaType: String): String {
    require(mediaType in listOf("image/svg+xml", "image/gif"))
    val data = Base64.encodeToString(bytes, Base64.NO_WRAP)
    return "<!doctype html><meta name=\"viewport\" content=\"width=device-width\">" +
        "<body style=\"margin:0;display:grid;place-items:center;min-height:100vh\">" +
        "<img alt=\"\" style=\"max-width:100%;max-height:100vh\" src=\"data:$mediaType;base64,$data\">"
}

/** Renders untrusted HTML without JavaScript, network, file or content access. */
@Composable
internal fun InertWebPage(html: String, description: String, modifier: Modifier = Modifier) {
    key(html) {
        AndroidView(
            factory = { context ->
                WebView(context).apply {
                    // WRAP_CONTENT makes WebView use a zero CSS viewport height, collapsing
                    // vh-sized images.
                    layoutParams =
                        ViewGroup.LayoutParams(
                            ViewGroup.LayoutParams.MATCH_PARENT,
                            ViewGroup.LayoutParams.MATCH_PARENT,
                        )
                    contentDescription = description
                    settings.javaScriptEnabled = false
                    settings.blockNetworkLoads = true
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    webViewClient =
                        object : WebViewClient() {
                            override fun shouldOverrideUrlLoading(
                                view: WebView,
                                request: WebResourceRequest,
                            ) = true

                            override fun shouldInterceptRequest(
                                view: WebView,
                                request: WebResourceRequest,
                            ): WebResourceResponse? =
                                if (request.url.scheme == "data") null
                                else WebResourceResponse("text/plain", "utf-8", null)
                        }
                    loadDataWithBaseURL(null, html, "text/html", "utf-8", null)
                }
            },
            onRelease = { it.destroy() },
            modifier = modifier.fillMaxSize(),
        )
    }
}

/** Shares image zoom/pan and pager arbitration across bitmap and WebView previews. */
@Composable
internal fun ZoomableImage(
    active: Boolean,
    zoomChanged: (Boolean) -> Unit,
    content: @Composable (Modifier) -> Unit,
) {
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    val transform = rememberTransformableState { zoom, pan, _ ->
        scale = (scale * zoom).coerceIn(1f, 6f)
        offset = if (scale == 1f) Offset.Zero else offset + pan
    }
    LaunchedEffect(active, scale) { if (active) zoomChanged(scale > 1f) }
    LaunchedEffect(active) {
        if (!active) {
            scale = 1f
            offset = Offset.Zero
        }
    }
    DisposableEffect(Unit) { onDispose { zoomChanged(false) } }
    Box(Modifier.fillMaxSize()) {
        content(
            Modifier.fillMaxSize().graphicsLayer {
                scaleX = scale
                scaleY = scale
                translationX = offset.x
                translationY = offset.y
            }
        )
        // An overlay receives gestures before the embedded WebView can consume them.
        Box(
            Modifier.matchParentSize()
                .testTag("artifact-image-gestures")
                .transformable(transform, canPan = { scale > 1f })
        )
    }
}
