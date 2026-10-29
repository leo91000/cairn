@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.DashPathEffect
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.Typeface
import android.widget.Toast
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import androidx.core.content.res.ResourcesCompat
import androidx.core.graphics.createBitmap
import androidx.core.graphics.withScale
import androidx.core.graphics.withTranslation
import build.cairn.app.R
import java.io.File
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sqrt
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

// Native drawing of Mermaid fences: the card in Markdown and its full-screen viewer. The web
// twin is apps/web/src/diagram-svg.ts, with Markdown.vue and DiagramViewer.vue around it.

/** A closed Mermaid fence, laid out once and drawn by the card and the viewer. */
internal class DiagramRender(
    val source: String,
    val kind: DiagramKind,
    val scene: Scene?,
    val error: String?,
)

/** Supported kinds draw or explain their failure; other sources stay code blocks. */
internal fun renderDiagram(source: String, measure: TextMeasure): DiagramRender? {
    val kind = diagramKind(source) ?: return null
    return try {
        val scene = layoutDiagram(parseDiagram(source), measure)
        // A chart without content to place has no finite extent to draw.
        if (
            scene.width.isFinite() && scene.height.isFinite() && scene.width > 0 && scene.height > 0
        )
            DiagramRender(source, kind, scene, null)
        else DiagramRender(source, kind, null, "This diagram has nothing to draw.")
    } catch (e: Exception) {
        DiagramRender(source, kind, null, e.message ?: e.toString())
    }
}

/** Owned by one Markdown renderer: unchanged diagrams keep their parsed layout. */
internal class DiagramCache(private val measure: TextMeasure) {
    private val entries =
        object : LinkedHashMap<String, DiagramRender>(16, 0.75f, true) {
            override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, DiagramRender>) =
                size > 32
        }

    fun render(source: String): DiagramRender? =
        entries[source] ?: renderDiagram(source, measure)?.also { entries[source] = it }
}

/**
 * The app's body font at the four weights diagrams use. Layout units are dp, measured at the
 * screen's density so drawn text matches the measured widths.
 */
internal class DiagramFonts private constructor(context: Context) : TextMeasure {
    private val density = context.resources.displayMetrics.density
    private val typefaces =
        listOf(
                400 to R.font.cairn_body_400,
                500 to R.font.cairn_body_500,
                600 to R.font.cairn_body_600,
                700 to R.font.cairn_body_700,
            )
            .associate { (weight, font) ->
                weight to
                    (runCatching { ResourcesCompat.getFont(context, font) }.getOrNull()
                        ?: if (weight >= 600) Typeface.DEFAULT_BOLD else Typeface.DEFAULT)
            }
    private val paint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG)
    private val widths = HashMap<String, Double>()

    fun typeface(weight: Int): Typeface =
        typefaces[weight] ?: typefaces.minBy { abs(it.key - weight) }.value

    @Synchronized
    override fun width(text: String, size: Double, weight: Int): Double {
        val key = "$size:$weight:$text"
        widths[key]?.let {
            return it
        }
        paint.typeface = typeface(weight)
        paint.textSize = (size * density).toFloat()
        val width = paint.measureText(text).toDouble() / density
        if (widths.size > 4096) widths.clear()
        widths[key] = width
        return width
    }

    companion object {
        private val instances = HashMap<Float, DiagramFonts>()

        fun get(context: Context): DiagramFonts =
            synchronized(instances) {
                instances.getOrPut(context.resources.displayMetrics.density) {
                    DiagramFonts(context.applicationContext)
                }
            }
    }
}

/** The web's light and dark diagram palettes (`--dg-*` in content.css), as ARGB. */
internal class DiagramColors(
    private val tones: Map<String, Int>,
    val canvas: Int,
    val dots: Int,
    val header: Int,
    val shadow: Int,
) {
    fun tone(name: String) = tones[name] ?: tones.getValue("ink")

    fun resolve(paint: DiagramPaint): Int? =
        when (paint) {
            is DiagramPaint.Tone -> tone(paint.tone)
            is DiagramPaint.Literal -> cssColor(paint.color, tone("ink"))
        }

    companion object {
        private fun palette(entries: String): Map<String, Int> =
            entries.trim().lines().associate { line ->
                val (name, value) = line.trim().split(' ')
                name to (0xFF000000 or value.toLong(16)).toInt()
            }

        val Light =
            DiagramColors(
                palette(
                    """
                    canvas fbfaf7
                    label fbfaf7
                    ink 16151d
                    muted 6d6a78
                    edge 8a8694
                    node ffffff
                    node-line cfc9bb
                    node-ink 16151d
                    terminal e4edf6
                    terminal-line 9dbbdc
                    terminal-ink 14365a
                    decision f1ecfa
                    decision-line b9a5dd
                    decision-ink 3f2a6b
                    data e3f3ef
                    data-line 8fcbbd
                    data-ink 0f5a4e
                    note fff4cf
                    note-line e0c873
                    note-ink 5c4813
                    cluster f3f1ea
                    cluster-line dcd7ca
                    cluster-ink 6d6a78
                    series-0 3b7bbf
                    series-1 e2512f
                    series-2 12806f
                    series-3 7a5bbf
                    series-4 c98a12
                    series-5 2a9db0
                    series-6 c2457d
                    series-7 5d6b82
                    """
                ),
                canvas = 0xFFFBFAF7.toInt(),
                dots = 0x1416151D,
                header = 0xF0FFFFFF.toInt(),
                shadow = 0x1416151D,
            )

        val Dark =
            DiagramColors(
                palette(
                    """
                    canvas 131319
                    label 131319
                    ink f3f2f7
                    muted 9d9bab
                    edge 7d7b8c
                    node 1e1e26
                    node-line 3a3a47
                    node-ink f3f2f7
                    terminal 1d2f44
                    terminal-line 3d6894
                    terminal-ink cfe2f6
                    decision 2c2638
                    decision-line 6c5a8e
                    decision-ink e5d8f3
                    data 16302b
                    data-line 2f6f62
                    data-ink bfeadf
                    note 353021
                    note-line 6e5f35
                    note-ink f1e2b6
                    cluster 18181f
                    cluster-line 33333f
                    cluster-ink a3a1b1
                    series-0 8db8e8
                    series-1 ff7b5e
                    series-2 4fc7a8
                    series-3 ceb5e5
                    series-4 dfc38e
                    series-5 7fd3e0
                    series-6 f29cc2
                    series-7 a7b3c7
                    """
                ),
                canvas = 0xFF131319.toInt(),
                dots = 0x0FFFFFFF,
                header = 0xF017171E.toInt(),
                shadow = 0x66000000,
            )
    }
}

/** Diagrams follow the app's own light or dark decision. */
@Composable
private fun diagramColors() =
    if (LocalCairnSignal.current == DarkSignal) DiagramColors.Dark else DiagramColors.Light

/**
 * Draws a scene in its own units on an Android canvas, for the card, the viewer and PNG export
 * alike. Paths are built once; paints are reused, so one drawing serves one thread.
 */
internal class SceneDrawing(val scene: Scene, private val fonts: DiagramFonts) {
    private val paths =
        scene.items.map { item ->
            (item as? PathItem)?.let { path ->
                Path().apply {
                    for (command in path.d) {
                        when (command) {
                            is PathCommand.Move -> moveTo(command.x.toFloat(), command.y.toFloat())
                            is PathCommand.Line -> lineTo(command.x.toFloat(), command.y.toFloat())
                            is PathCommand.Cubic ->
                                cubicTo(
                                    command.x1.toFloat(),
                                    command.y1.toFloat(),
                                    command.x2.toFloat(),
                                    command.y2.toFloat(),
                                    command.x.toFloat(),
                                    command.y.toFloat(),
                                )
                            PathCommand.Close -> close()
                        }
                    }
                }
            }
        }

    /** Tap targets for nodes with an http(s) link, in scene units. */
    val links: List<Pair<RectF, String>> =
        scene.items.mapNotNull { item ->
            val link = (item as? PathItem)?.link ?: return@mapNotNull null
            val bounds = RectF(Float.MAX_VALUE, Float.MAX_VALUE, -Float.MAX_VALUE, -Float.MAX_VALUE)
            fun include(x: Double, y: Double) = bounds.union(x.toFloat(), y.toFloat())
            for (command in item.d) {
                when (command) {
                    is PathCommand.Move -> include(command.x, command.y)
                    is PathCommand.Line -> include(command.x, command.y)
                    is PathCommand.Cubic -> {
                        include(command.x1, command.y1)
                        include(command.x2, command.y2)
                        include(command.x, command.y)
                    }
                    PathCommand.Close -> Unit
                }
            }
            bounds to link
        }

    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.FILL }
    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val text =
        Paint(Paint.ANTI_ALIAS_FLAG or Paint.SUBPIXEL_TEXT_FLAG or Paint.LINEAR_TEXT_FLAG)
    private val shadow = Path()

    fun draw(canvas: android.graphics.Canvas, colors: DiagramColors) {
        scene.items.forEachIndexed { index, item ->
            when (item) {
                is PathItem -> drawPath(canvas, paths[index]!!, item, colors)
                is TextItem -> drawText(canvas, item, colors)
            }
        }
    }

    private fun withOpacity(color: Int, opacity: Double): Int {
        if (opacity >= 1) return color
        val alpha = ((color ushr 24) * opacity.coerceIn(0.0, 1.0)).roundToInt()
        return (alpha shl 24) or (color and 0xFFFFFF)
    }

    private fun drawPath(
        canvas: android.graphics.Canvas,
        path: Path,
        item: PathItem,
        colors: DiagramColors,
    ) {
        val opacity = item.opacity ?: 1.0
        // An unparsable author fill falls back to black, an unparsable stroke to none, as in SVG.
        val fillColor = item.fill?.let { colors.resolve(it) ?: 0xFF000000.toInt() }
        if (item.shadow && fillColor != null) {
            // A soft drop shadow of the web's 0 1px 1.5px, layered so it reads on any canvas.
            for ((offset, strength) in listOf(1.6f to 0.45, 1.0f to 0.75, 0.5f to 1.0)) {
                path.offset(0f, offset, shadow)
                fill.color = withOpacity(colors.shadow, strength * opacity)
                canvas.drawPath(shadow, fill)
            }
        }
        if (fillColor != null) {
            fill.color = withOpacity(fillColor, opacity)
            canvas.drawPath(path, fill)
        }
        val strokeColor = item.stroke?.let { colors.resolve(it) } ?: return
        stroke.color = withOpacity(strokeColor, opacity)
        stroke.strokeWidth = (item.width?.takeIf { it > 0 } ?: 1.0).toFloat()
        stroke.pathEffect =
            item.dash?.let { DashPathEffect(it.map(Double::toFloat).toFloatArray(), 0f) }
        canvas.drawPath(path, stroke)
    }

    private fun drawText(canvas: android.graphics.Canvas, item: TextItem, colors: DiagramColors) {
        text.color = colors.resolve(item.fill) ?: 0xFF000000.toInt()
        text.typeface = fonts.typeface(item.weight)
        text.textSize = item.size.toFloat()
        text.textAlign =
            when (item.anchor) {
                TextAnchor.START -> Paint.Align.LEFT
                TextAnchor.MIDDLE -> Paint.Align.CENTER
                TextAnchor.END -> Paint.Align.RIGHT
            }
        // Each line is centered on its line box, like SVG's central baseline.
        val metrics = text.fontMetrics
        val central = (metrics.ascent + metrics.descent) / 2
        val height = lineHeight(item.size)
        item.lines.forEachIndexed { index, line ->
            val y = item.y + (index - (item.lines.size - 1) / 2.0) * height
            canvas.drawText(line, item.x.toFloat(), (y - central).toFloat(), text)
        }
    }
}

private fun DiagramKind.label() =
    when (this) {
        DiagramKind.FLOWCHART -> "Logigramme"
        DiagramKind.STATE -> "Diagramme d’états"
        DiagramKind.SEQUENCE -> "Diagramme de séquence"
        DiagramKind.PIE -> "Graphique circulaire"
    }

private fun DiagramKind.icon() =
    when (this) {
        DiagramKind.FLOWCHART,
        DiagramKind.STATE -> CairnIcons.Workflow
        DiagramKind.SEQUENCE -> CairnIcons.Sequence
        DiagramKind.PIE -> CairnIcons.Pie
    }

/** The web's dotted canvas: one faint dot centered in every square of the grid. */
private fun DrawScope.dotGrid(color: Int, spacing: Float, radius: Float) {
    val dot = Color(color)
    var y = spacing / 2
    while (y < size.height) {
        var x = spacing / 2
        while (x < size.width) {
            drawCircle(dot, radius, Offset(x, y))
            x += spacing
        }
        y += spacing
    }
}

private fun copySource(context: Context, source: String) {
    context
        .getSystemService(ClipboardManager::class.java)
        .setPrimaryClip(ClipData.newPlainText("Diagramme", source))
}

@Composable
private fun DiagramAction(
    label: String,
    icon: ImageVector,
    checked: Boolean? = null,
    onClick: () -> Unit,
) {
    TooltipBox(
        positionProvider =
            TooltipDefaults.rememberTooltipPositionProvider(TooltipAnchorPosition.Above),
        tooltip = { PlainTooltip { Text(label) } },
        state = rememberTooltipState(),
    ) {
        val content = MaterialTheme.colorScheme.onSurfaceVariant
        if (checked == null) {
            IconButton(
                onClick = onClick,
                colors = IconButtonDefaults.iconButtonColors(contentColor = content),
            ) {
                Icon(icon, label, Modifier.size(18.dp))
            }
        } else {
            IconToggleButton(
                checked = checked,
                onCheckedChange = { onClick() },
                colors =
                    IconButtonDefaults.iconToggleButtonColors(
                        contentColor = content,
                        checkedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                        checkedContentColor = MaterialTheme.colorScheme.onSurface,
                    ),
            ) {
                Icon(icon, label, Modifier.size(18.dp))
            }
        }
    }
}

/** A drawn diagram, its header actions and, on request or failure, its source. */
@Composable
internal fun DiagramFigure(
    diagram: DiagramRender,
    showSource: Boolean,
    onShowSource: (Boolean) -> Unit,
    openLink: (String) -> Unit,
) {
    val context = LocalContext.current
    val colors = diagramColors()
    val line = MaterialTheme.colorScheme.outlineVariant
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val shape = RoundedCornerShape(14.dp)
    val scene = diagram.scene
    var copied by remember { mutableStateOf(false) }
    var viewing by remember { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }

    Column(
        Modifier.fillMaxWidth()
            .testTag("diagram")
            .clip(shape)
            .border(1.dp, line, shape)
            .background(Color(colors.canvas))
    ) {
        Row(
            Modifier.fillMaxWidth()
                .background(Color(colors.header))
                .padding(start = 14.dp, end = 2.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(diagram.kind.icon(), null, Modifier.size(16.dp), tint = muted)
            Spacer(Modifier.width(7.dp))
            Text(
                diagram.kind.label(),
                Modifier.weight(1f),
                style = MaterialTheme.typography.labelMedium,
                fontWeight = FontWeight.SemiBold,
                letterSpacing = 0.12.sp,
                color = muted,
            )
            if (scene != null)
                DiagramAction("Afficher le code", CairnIcons.Code, checked = showSource) {
                    onShowSource(!showSource)
                }
            DiagramAction(
                if (copied) "Copié" else "Copier le code",
                if (copied) CairnIcons.Check else CairnIcons.Copy,
            ) {
                copySource(context, diagram.source)
                copied = true
            }
            if (scene != null) DiagramAction("Agrandir", CairnIcons.Expand) { viewing = true }
        }
        HorizontalDivider(color = line)

        if (scene != null) {
            DiagramCanvas(scene, colors, openLink)
        } else {
            Text(
                "Impossible de dessiner ce diagramme. ${diagram.error.orEmpty()}".trim(),
                Modifier.padding(start = 14.dp, end = 14.dp, top = 10.dp, bottom = 10.dp),
                style = MaterialTheme.typography.bodySmall,
                fontSize = 12.5.sp,
                color = muted,
            )
        }

        // A failed drawing keeps its source in view; it is all the reader has.
        if (showSource || scene == null) {
            if (scene != null) HorizontalDivider(color = line)
            SelectionContainer {
                Text(
                    diagram.source,
                    Modifier.fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surfaceContainer)
                        .padding(16.dp)
                        .testTag("diagram-source"),
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.5.sp,
                    lineHeight = 19.sp,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
        }
    }

    if (viewing && scene != null) DiagramViewer(diagram, scene, close = { viewing = false })
}

/**
 * Wide drawings shrink to the message, but not below a legible size; they scroll instead. Laid out
 * by the renderer beforehand, so the card keeps its height from the first frame.
 */
@Composable
private fun DiagramCanvas(scene: Scene, colors: DiagramColors, openLink: (String) -> Unit) {
    val context = LocalContext.current
    val drawing = remember(scene) { SceneDrawing(scene, DiagramFonts.get(context)) }
    val dotSpacing = with(LocalDensity.current) { 16.dp.toPx() }
    val dotRadius = with(LocalDensity.current) { 1.dp.toPx() }
    BoxWithConstraints(
        Modifier.fillMaxWidth()
            .drawBehind { dotGrid(colors.dots, dotSpacing, dotRadius) }
            .padding(horizontal = 12.dp, vertical = 18.dp)
    ) {
        val available = maxWidth.value.toDouble()
        val legible = min(scene.width, max(320.0, scene.width * 0.75))
        val width = min(scene.width, max(available, legible))
        val ratio = width / scene.width
        val scrolls = width > available + 0.5
        Box(
            Modifier.fillMaxWidth()
                .then(if (scrolls) Modifier.horizontalScroll(rememberScrollState()) else Modifier),
            contentAlignment = Alignment.TopCenter,
        ) {
            Box(Modifier.size(width.dp, (scene.height * ratio).dp)) {
                Canvas(
                    Modifier.fillMaxSize().testTag("diagram-drawing").semantics {
                        contentDescription = scene.label
                    }
                ) {
                    drawIntoCanvas { canvas ->
                        canvas.nativeCanvas.withScale(
                            size.width / scene.width.toFloat(),
                            size.height / scene.height.toFloat(),
                        ) {
                            drawing.draw(this, colors)
                        }
                    }
                }
                for ((bounds, link) in drawing.links) {
                    Box(
                        Modifier.offset((bounds.left * ratio).dp, (bounds.top * ratio).dp)
                            .size((bounds.width() * ratio).dp, (bounds.height() * ratio).dp)
                            .clip(RoundedCornerShape(8.dp))
                            .clickable(role = Role.Button, onClickLabel = "Ouvrir le lien") {
                                openLink(link)
                            }
                            .semantics { contentDescription = link }
                    )
                }
            }
        }
    }
}

private const val MIN_ZOOM = 0.15f
private const val MAX_ZOOM = 6f

/** Full-screen view: pinch to zoom, drag to move, double tap to fit, share as an image. */
@Composable
private fun DiagramViewer(diagram: DiagramRender, scene: Scene, close: () -> Unit) {
    val context = LocalContext.current
    val density = LocalDensity.current.density
    val colors = diagramColors()
    val drawing = remember(scene) { SceneDrawing(scene, DiagramFonts.get(context)) }
    val scope = rememberCoroutineScope()
    var stage by remember { mutableStateOf(IntSize.Zero) }
    var scale by remember { mutableFloatStateOf(1f) }
    var offset by remember { mutableStateOf(Offset.Zero) }
    var fitted by remember { mutableStateOf(true) }
    var copied by remember { mutableStateOf(false) }
    var sharing by remember { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(1500)
            copied = false
        }
    }

    fun fit() {
        if (stage == IntSize.Zero) return
        val padding = 32 * density
        val width = scene.width.toFloat() * density
        val height = scene.height.toFloat() * density
        val next =
            min(min((stage.width - padding * 2) / width, (stage.height - padding * 2) / height), 2f)
                .coerceIn(MIN_ZOOM, MAX_ZOOM)
        scale = next
        offset = Offset((stage.width - width * next) / 2, (stage.height - height * next) / 2)
        fitted = true
    }

    /** Zoom around a point of the stage, keeping the drawing under it in place. */
    fun zoomAt(next: Float, pivot: Offset) {
        val target = next.coerceIn(MIN_ZOOM, MAX_ZOOM)
        offset = pivot - (pivot - offset) * (target / scale)
        scale = target
        fitted = false
    }

    fun zoomBy(factor: Float) = zoomAt(scale * factor, Offset(stage.width / 2f, stage.height / 2f))

    LaunchedEffect(stage) { if (fitted) fit() }

    Dialog(
        onDismissRequest = close,
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(diagram.kind.label()) },
                    navigationIcon = {
                        IconButton(onClick = close) { Icon(CairnIcons.Close, "Fermer") }
                    },
                    actions = {
                        ActionIcon(
                            if (copied) "Copié" else "Copier le code",
                            if (copied) CairnIcons.Check else CairnIcons.Copy,
                        ) {
                            copySource(context, diagram.source)
                            copied = true
                        }
                        ActionIcon(
                            "Partager l’image",
                            Icons.Default.Share,
                            enabled = !sharing,
                        ) {
                            sharing = true
                            scope.launch {
                                try {
                                    shareDiagramImage(context, scene, colors, diagram.kind)
                                } catch (e: CancellationException) {
                                    throw e
                                } catch (e: Exception) {
                                    Toast.makeText(
                                            context,
                                            "Impossible de partager l’image : ${e.message}",
                                            Toast.LENGTH_LONG,
                                        )
                                        .show()
                                } finally {
                                    sharing = false
                                }
                            }
                        }
                    },
                )
            }
        ) { padding ->
            Box(
                Modifier.padding(padding)
                    .fillMaxSize()
                    .background(Color(colors.canvas))
                    .drawBehind {
                        dotGrid(colors.dots, 18.dp.toPx(), 1.dp.toPx())
                    }
                    .clipToBounds()
                    .onSizeChanged { stage = it }
                    .pointerInput(Unit) {
                        detectTransformGestures { centroid, pan, zoom, _ ->
                            zoomAt(scale * zoom, centroid)
                            offset += pan
                            fitted = false
                        }
                    }
                    .pointerInput(Unit) { detectTapGestures(onDoubleTap = { fit() }) }
                    .testTag("diagram-stage")
            ) {
                Canvas(Modifier.fillMaxSize().semantics { contentDescription = scene.label }) {
                    drawIntoCanvas { canvas ->
                        canvas.nativeCanvas.withTranslation(offset.x, offset.y) {
                            scale(scale * density, scale * density)
                            drawing.draw(this, colors)
                        }
                    }
                }
                Surface(
                    Modifier.align(Alignment.BottomCenter).navigationBarsPadding().padding(16.dp),
                    shape = CircleShape,
                    color = MaterialTheme.colorScheme.surface,
                    shadowElevation = 3.dp,
                    border =
                        androidx.compose.foundation.BorderStroke(
                            1.dp,
                            MaterialTheme.colorScheme.outlineVariant,
                        ),
                ) {
                    Row(
                        Modifier.padding(horizontal = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        ActionIcon("Zoom arrière", CairnIcons.ZoomOut) { zoomBy(0.8f) }
                        TextButton(
                            onClick = ::fit,
                            modifier =
                                Modifier.widthIn(min = 64.dp).semantics {
                                    contentDescription = "Ajuster à l’écran"
                                },
                        ) {
                            Text(
                                "${(scale * 100).roundToInt()} %",
                                style = MaterialTheme.typography.labelLarge,
                                fontWeight = FontWeight.SemiBold,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        ActionIcon("Zoom avant", CairnIcons.ZoomIn) { zoomBy(1.25f) }
                        ActionIcon("Ajuster à l’écran", CairnIcons.Fit, onClick = ::fit)
                    }
                }
            }
        }
    }
}

private fun DiagramKind.fileName() =
    when (this) {
        DiagramKind.FLOWCHART -> "organigramme"
        DiagramKind.STATE -> "diagramme-etats"
        DiagramKind.SEQUENCE -> "diagramme-sequence"
        DiagramKind.PIE -> "graphique-circulaire"
    }

/** Twice the drawing's size, bounded so a huge chart still fits in memory. */
internal fun diagramImage(context: Context, scene: Scene, colors: DiagramColors): Bitmap {
    val area = scene.width * scene.height
    val ratio = min(2.0, min(sqrt(16_000_000 / area), 8192 / max(scene.width, scene.height)))
    val bitmap =
        createBitmap(
            max(1, (scene.width * ratio).roundToInt()),
            max(1, (scene.height * ratio).roundToInt()),
        )
    val canvas = android.graphics.Canvas(bitmap)
    canvas.drawColor(colors.canvas)
    canvas.scale(ratio.toFloat(), ratio.toFloat())
    SceneDrawing(scene, DiagramFonts.get(context)).draw(canvas, colors)
    return bitmap
}

/** Shares a PNG through the app's file provider, like downloaded attachments. */
private suspend fun shareDiagramImage(
    context: Context,
    scene: Scene,
    colors: DiagramColors,
    kind: DiagramKind,
) {
    val name = "${kind.fileName()}.png"
    val file =
        withContext(Dispatchers.Default) {
            val bitmap = diagramImage(context, scene, colors)
            try {
                File(context.cacheDir, "cairn-files")
                    .apply { mkdirs() }
                    .resolve(name)
                    .also { file ->
                        file.outputStream().use {
                            bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
                        }
                    }
            } finally {
                bitmap.recycle()
            }
        }
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file, name)
    val intent =
        Intent(Intent.ACTION_SEND)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            .setType("image/png")
            .putExtra(Intent.EXTRA_STREAM, uri)
            .putExtra(Intent.EXTRA_TITLE, name)
    intent.clipData = ClipData.newRawUri(name, uri)
    context.startActivity(Intent.createChooser(intent, "Partager l’image"))
}
