package build.cairn.app.ui

import kotlin.math.PI
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.math.sin
import kotlin.math.sqrt
import kotlin.math.tan

// Lays a parsed diagram out as a flat scene of paths and text. Renderers only map tones to
// theme colors, so Android and the web (apps/web/src/diagram-layout.ts) draw the same geometry.
// The arithmetic follows the web operation by operation: tests/fixtures/diagrams.json pins it.

internal sealed interface PathCommand {
    data class Move(val x: Double, val y: Double) : PathCommand

    data class Line(val x: Double, val y: Double) : PathCommand

    data class Cubic(
        val x1: Double,
        val y1: Double,
        val x2: Double,
        val y2: Double,
        val x: Double,
        val y: Double,
    ) : PathCommand

    data object Close : PathCommand
}

/** A theme role, resolved by renderers to a light or dark color, or an author's CSS color. */
internal sealed interface DiagramPaint {
    data class Tone(val tone: String) : DiagramPaint

    data class Literal(val color: String) : DiagramPaint
}

internal enum class TextAnchor {
    START,
    MIDDLE,
    END,
}

internal sealed interface SceneItem

internal data class PathItem(
    val d: List<PathCommand>,
    val fill: DiagramPaint? = null,
    val stroke: DiagramPaint? = null,
    val width: Double? = null,
    val dash: List<Double>? = null,
    val opacity: Double? = null,
    val link: String? = null,
    /** Raised node shapes cast a soft shadow. */
    val shadow: Boolean = false,
) : SceneItem

internal data class TextItem(
    val x: Double,
    /** Center of the text block; each line is centered on its own line box. */
    val y: Double,
    val lines: List<String>,
    val size: Double,
    val weight: Int,
    val anchor: TextAnchor,
    val fill: DiagramPaint,
    val link: String? = null,
) : SceneItem

internal class Scene(
    val width: Double,
    val height: Double,
    val items: List<SceneItem>,
    /** Short accessible description of the drawing. */
    val label: String,
)

internal fun interface TextMeasure {
    fun width(text: String, size: Double, weight: Int): Double
}

internal fun lineHeight(size: Double) = jsRound(size * 1.4)

private fun tone(name: String) = DiagramPaint.Tone(name)

private const val NODE_FONT = 14.0
private const val LABEL_FONT = 12.0
private const val TITLE_FONT = 16.0
private const val CLUSTER_FONT = 12.0
private const val WRAP = 200.0
private const val MARGIN = 12.0
private const val NODE_SEP = 36.0
private const val EDGE_SEP = 16.0
private const val RANK_GAP = 26.0
private const val CLUSTER_PAD = 16.0
private const val ARROW = 8.0
internal const val MAX_LAYOUT_NODES = 4000
private const val MAX_LAYOUT_CONSTRAINTS = 10000
private const val MAX_SCENE_SIDE = 32768

internal fun layoutDiagram(diagram: Diagram, measure: TextMeasure): Scene {
    val scene =
        when (diagram) {
            is GraphDiagram -> layoutGraph(diagram, measure)
            is SequenceDiagram -> layoutSequence(diagram, measure)
            is PieDiagram -> layoutPie(diagram, measure)
        }
    if (scene.width > MAX_SCENE_SIDE || scene.height > MAX_SCENE_SIDE)
        throw DiagramError("This diagram is too large to draw.")
    return scene
}

private val WORDS = Regex("$SPACE+")

/** Break on explicit newlines, then between words to fit `max`, then inside long words. */
internal fun wrapText(
    text: String,
    size: Double,
    weight: Int,
    max: Double,
    measure: TextMeasure,
): List<String> {
    val lines = mutableListOf<String>()
    for (paragraph in text.split('\n')) {
        var line = ""
        for (word in paragraph.split(WORDS).filter { it.isNotEmpty() }) {
            val candidate = if (line.isNotEmpty()) "$line $word" else word
            if (measure.width(candidate, size, weight) <= max) {
                line = candidate
                continue
            }

            if (line.isNotEmpty()) lines.add(line)
            line = ""
            var start = 0
            while (start < word.length) {
                val remaining = word.length - start
                var low = 1
                var high = min(2, remaining)
                // Grow from the start of each segment, never rescan the entire remaining word.
                while (
                    high < remaining &&
                        measure.width(word.substring(start, start + high), size, weight) <= max
                ) {
                    low = high
                    high = min(high * 2, remaining)
                }
                if (measure.width(word.substring(start, start + high), size, weight) <= max) {
                    low = high
                } else {
                    while (high - low > 1) {
                        val middle = (low + high) / 2
                        if (
                            measure.width(word.substring(start, start + middle), size, weight) <=
                                max
                        )
                            low = middle
                        else high = middle
                    }
                }
                val segment = word.substring(start, start + low)
                start += low
                if (start < word.length) lines.add(segment) else line = segment
            }
        }

        lines.add(line)
    }

    while (lines.size > 1 && lines.last().isEmpty()) lines.removeAt(lines.size - 1)
    return lines
}

private class TextBlock(val lines: List<String>, val width: Double, val height: Double)

private fun textBlock(
    text: String,
    size: Double,
    weight: Int,
    max: Double,
    measure: TextMeasure,
): TextBlock {
    val lines = if (text.isNotEmpty()) wrapText(text, size, weight, max, measure) else emptyList()
    val width = lines.fold(0.0) { widest, line -> max(widest, measure.width(line, size, weight)) }
    return TextBlock(lines, width, lines.size * lineHeight(size))
}

/** `Math.min(...values)`: infinite when empty. */
private fun minimum(values: Iterable<Double>) = values.fold(Double.POSITIVE_INFINITY, ::min)

/** `Math.max(...values)`: negative infinite when empty. */
private fun maximum(values: Iterable<Double>) = values.fold(Double.NEGATIVE_INFINITY, ::max)

/** A JavaScript sort comparator's result. */
private fun sortOrder(difference: Double) = if (difference < 0) -1 else if (difference > 0) 1 else 0

// Path helpers. Quarter ellipse arcs use the usual cubic approximation.
private const val KAPPA = 0.5523

private fun roundedRect(
    x: Double,
    y: Double,
    w: Double,
    h: Double,
    radius: Double,
): List<PathCommand> {
    val r = max(0.0, min(min(radius, w / 2), h / 2))
    val k = r * (1 - KAPPA)
    return listOf(
        PathCommand.Move(x + r, y),
        PathCommand.Line(x + w - r, y),
        PathCommand.Cubic(x + w - k, y, x + w, y + k, x + w, y + r),
        PathCommand.Line(x + w, y + h - r),
        PathCommand.Cubic(x + w, y + h - k, x + w - k, y + h, x + w - r, y + h),
        PathCommand.Line(x + r, y + h),
        PathCommand.Cubic(x + k, y + h, x, y + h - k, x, y + h - r),
        PathCommand.Line(x, y + r),
        PathCommand.Cubic(x, y + k, x + k, y, x + r, y),
        PathCommand.Close,
    )
}

private fun ellipse(cx: Double, cy: Double, rx: Double, ry: Double): List<PathCommand> {
    val ox = rx * KAPPA
    val oy = ry * KAPPA
    return listOf(
        PathCommand.Move(cx - rx, cy),
        PathCommand.Cubic(cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry),
        PathCommand.Cubic(cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy),
        PathCommand.Cubic(cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry),
        PathCommand.Cubic(cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy),
        PathCommand.Close,
    )
}

private class Pt(val x: Double, val y: Double)

private fun polygon(vararg points: Pt): List<PathCommand> =
    listOf(PathCommand.Move(points[0].x, points[0].y)) +
        points.drop(1).map { PathCommand.Line(it.x, it.y) } +
        PathCommand.Close

/** Arc from angle a0 to a1 (radians, clockwise from 12 o'clock), as cubic segments. */
private fun arc(
    cx: Double,
    cy: Double,
    r: Double,
    a0: Double,
    a1: Double,
    move: Boolean,
): List<PathCommand> {
    val commands = mutableListOf<PathCommand>()
    fun point(angle: Double) = Pt(cx + r * sin(angle), cy - r * cos(angle))
    val steps = max(1.0, ceil(abs(a1 - a0) / (PI / 2))).toInt()
    val step = (a1 - a0) / steps
    val handle = (4.0 / 3) * tan(step / 4) * r
    var current = point(a0)
    commands.add(
        if (move) PathCommand.Move(current.x, current.y) else PathCommand.Line(current.x, current.y)
    )
    for (index in 0 until steps) {
        val start = a0 + step * index
        val end = start + step
        val next = point(end)
        commands.add(
            PathCommand.Cubic(
                current.x + handle * cos(start),
                current.y + handle * sin(start),
                next.x - handle * cos(end),
                next.y - handle * sin(end),
                next.x,
                next.y,
            )
        )
        current = next
    }

    return commands
}

// Author colors stay readable: text on a custom fill picks ink by the fill's luminance.
private fun luminance(color: String): Double? {
    if (color.equals("currentcolor", ignoreCase = true)) return null
    val argb = cssColor(color, 0) ?: return null
    // A translucent fill depends on the theme beneath it; keep the theme's normal ink.
    if (argb ushr 24 != 255) return null
    val rgb = listOf((argb ushr 16) and 255, (argb ushr 8) and 255, argb and 255)
    val (r, g, b) =
        rgb.map { value ->
            val channel = value / 255.0
            if (channel <= 0.03928) channel / 12.92 else ((channel + 0.055) / 1.055).pow(2.4)
        }
    return 0.2126 * r + 0.7152 * g + 0.0722 * b
}

private fun textOn(style: DiagramStyle?, fallback: String): DiagramPaint {
    if (style?.color != null) return DiagramPaint.Literal(style.color)
    if (style?.fill != null) {
        val light = luminance(style.fill)
        if (light != null) {
            val darkInk = "#16151d"
            val darkContrast = (light + 0.05) / (luminance(darkInk)!! + 0.05)
            val lightContrast = 1.05 / (light + 0.05)
            return DiagramPaint.Literal(if (darkContrast >= lightContrast) darkInk else "#ffffff")
        }
    }

    return tone(fallback)
}

private fun family(shape: NodeShape) =
    when (shape) {
        NodeShape.STADIUM,
        NodeShape.CIRCLE,
        NodeShape.DOUBLE_CIRCLE -> "terminal"
        NodeShape.DIAMOND,
        NodeShape.HEXAGON,
        NodeShape.CHOICE -> "decision"
        NodeShape.CYLINDER -> "data"
        NodeShape.NOTE -> "note"
        else -> "node"
    }

private class NodeBox(val w: Double, val h: Double, val text: TextBlock)

private fun nodeBox(node: GraphNode, horizontal: Boolean, measure: TextMeasure): NodeBox {
    val text = textBlock(node.label, NODE_FONT, 500, WRAP, measure)
    val tw = text.width
    val th = max(text.height, lineHeight(NODE_FONT))
    fun box(w: Double, h: Double) = NodeBox(ceil(w), ceil(h), text)
    return when (node.shape) {
        NodeShape.START -> box(16.0, 16.0)
        NodeShape.END -> box(20.0, 20.0)
        NodeShape.FORK -> if (horizontal) box(8.0, 64.0) else box(64.0, 8.0)
        NodeShape.CHOICE -> box(28.0, 28.0)
        NodeShape.CIRCLE -> {
            val d = sqrt(tw * tw + th * th) + 28
            box(d, d)
        }
        NodeShape.DOUBLE_CIRCLE -> {
            val d = sqrt(tw * tw + th * th) + 38
            box(d, d)
        }
        NodeShape.DIAMOND -> {
            // Half extents (W, H) contain the label box when (tw/2)/W + (th/2)/H ≤ 1.
            val h = th / 2 + 22
            val w = (tw / 2 + 10) / (1 - (th / 2) / h)
            box(max(w * 2, 56.0), h * 2)
        }
        NodeShape.HEXAGON -> box(tw + 40 + th, th + 22)
        NodeShape.PARALLELOGRAM,
        NodeShape.PARALLELOGRAM_ALT,
        NodeShape.TRAPEZOID,
        NodeShape.TRAPEZOID_ALT -> box(tw + 36 + (th + 22) * 0.7, th + 22)
        NodeShape.CYLINDER -> box(max(tw + 32, 64.0), th + 34)
        NodeShape.SUBROUTINE -> box(tw + 48, th + 22)
        NodeShape.FLAG -> box(tw + 44, th + 22)
        NodeShape.STADIUM -> box(tw + 28 + th, th + 22)
        NodeShape.NOTE -> box(tw + 36, th + 24)
        else -> box(max(tw + 32, 56.0), th + 22)
    }
}

/** Distance from a node's center to its bottom (or top) outline at horizontal offset dx. */
private fun outline(shape: NodeShape, w: Double, h: Double, dx: Double): Double {
    val hw = w / 2
    val hh = h / 2
    val t = min(1.0, abs(dx) / hw)
    if (shape == NodeShape.DIAMOND || shape == NodeShape.CHOICE) return hh * (1 - t)
    if (
        shape == NodeShape.CIRCLE ||
            shape == NodeShape.DOUBLE_CIRCLE ||
            shape == NodeShape.START ||
            shape == NodeShape.END
    )
        return hh * sqrt(max(0.0, 1 - t * t))
    if (shape == NodeShape.STADIUM) {
        val flat = hw - hh
        if (abs(dx) <= flat) return hh
        val e = (abs(dx) - flat) / hh
        return hh * sqrt(max(0.0, 1 - e * e))
    }

    if (shape == NodeShape.HEXAGON) {
        val inset = h / 2
        val flat = hw - inset
        return if (abs(dx) <= flat) hh else hh * (hw - abs(dx)) / inset
    }

    return hh
}

private class ShapePaths(
    val outlines: List<List<PathCommand>>,
    val details: List<List<PathCommand>>,
)

private fun shapePaths(shape: NodeShape, cx: Double, cy: Double, w: Double, h: Double): ShapePaths {
    val x = cx - w / 2
    val y = cy - h / 2
    val r = x + w
    val b = y + h
    val outlines = mutableListOf<List<PathCommand>>()
    val details = mutableListOf<List<PathCommand>>()
    when (shape) {
        NodeShape.ROUND -> outlines.add(roundedRect(x, y, w, h, min(16.0, h / 2)))
        NodeShape.STADIUM -> outlines.add(roundedRect(x, y, w, h, h / 2))
        NodeShape.SUBROUTINE -> {
            outlines.add(roundedRect(x, y, w, h, 4.0))
            details.add(listOf(PathCommand.Move(x + 9, y), PathCommand.Line(x + 9, b)))
            details.add(listOf(PathCommand.Move(r - 9, y), PathCommand.Line(r - 9, b)))
        }
        NodeShape.CYLINDER -> {
            val ry = 7.0
            val k = (w / 2) * KAPPA
            val ok = ry * KAPPA
            outlines.add(
                listOf(
                    PathCommand.Move(x, y + ry),
                    PathCommand.Line(x, b - ry),
                    PathCommand.Cubic(x, b - ry + ok, cx - k, b, cx, b),
                    PathCommand.Cubic(cx + k, b, r, b - ry + ok, r, b - ry),
                    PathCommand.Line(r, y + ry),
                    PathCommand.Cubic(r, y + ry - ok, cx + k, y, cx, y),
                    PathCommand.Cubic(cx - k, y, x, y + ry - ok, x, y + ry),
                    PathCommand.Close,
                )
            )
            details.add(
                listOf(
                    PathCommand.Move(x, y + ry),
                    PathCommand.Cubic(x, y + ry + ok, cx - k, y + 2 * ry, cx, y + 2 * ry),
                    PathCommand.Cubic(cx + k, y + 2 * ry, r, y + ry + ok, r, y + ry),
                )
            )
        }
        NodeShape.CIRCLE,
        NodeShape.START -> outlines.add(ellipse(cx, cy, w / 2, h / 2))
        NodeShape.DOUBLE_CIRCLE,
        NodeShape.END -> {
            outlines.add(ellipse(cx, cy, w / 2, h / 2))
            details.add(ellipse(cx, cy, w / 2 - 5, h / 2 - 5))
        }
        NodeShape.DIAMOND,
        NodeShape.CHOICE -> outlines.add(polygon(Pt(cx, y), Pt(r, cy), Pt(cx, b), Pt(x, cy)))
        NodeShape.HEXAGON -> {
            val inset = h / 2
            outlines.add(
                polygon(
                    Pt(x + inset, y),
                    Pt(r - inset, y),
                    Pt(r, cy),
                    Pt(r - inset, b),
                    Pt(x + inset, b),
                    Pt(x, cy),
                )
            )
        }
        NodeShape.PARALLELOGRAM,
        NodeShape.PARALLELOGRAM_ALT,
        NodeShape.TRAPEZOID,
        NodeShape.TRAPEZOID_ALT -> {
            val s = h * 0.35
            outlines.add(
                when (shape) {
                    NodeShape.PARALLELOGRAM ->
                        polygon(Pt(x + s, y), Pt(r, y), Pt(r - s, b), Pt(x, b))
                    NodeShape.PARALLELOGRAM_ALT ->
                        polygon(Pt(x, y), Pt(r - s, y), Pt(r, b), Pt(x + s, b))
                    NodeShape.TRAPEZOID -> polygon(Pt(x + s, y), Pt(r - s, y), Pt(r, b), Pt(x, b))
                    else -> polygon(Pt(x, y), Pt(r, y), Pt(r - s, b), Pt(x + s, b))
                }
            )
        }
        NodeShape.FLAG ->
            outlines.add(polygon(Pt(x, y), Pt(r, y), Pt(r, b), Pt(x, b), Pt(x + h / 3, cy)))
        NodeShape.NOTE -> {
            val fold = 12.0
            outlines.add(polygon(Pt(x, y), Pt(r - fold, y), Pt(r, y + fold), Pt(r, b), Pt(x, b)))
            details.add(
                listOf(
                    PathCommand.Move(r - fold, y),
                    PathCommand.Line(r - fold, y + fold),
                    PathCommand.Line(r, y + fold),
                )
            )
        }
        NodeShape.FORK -> outlines.add(roundedRect(x, y, w, h, min(w, h) / 2))
        else -> outlines.add(roundedRect(x, y, w, h, 7.0))
    }

    return ShapePaths(outlines, details)
}

private fun headPaths(
    head: EdgeHead,
    tip: Pt,
    direction: Pt,
    paint: DiagramPaint,
    width: Double,
): List<PathItem> {
    val ux = direction.x
    val uy = direction.y
    val px = -uy
    val py = ux
    val tx = tip.x
    val ty = tip.y
    if (head == EdgeHead.ARROW) {
        val back = ARROW + width
        val half = 4.5 + width / 2
        val bx = tx - ux * back
        val by = ty - uy * back
        return listOf(
            PathItem(
                d =
                    listOf(
                        PathCommand.Move(tx, ty),
                        PathCommand.Line(bx + px * half, by + py * half),
                        PathCommand.Cubic(
                            bx + ux * 2,
                            by + uy * 2,
                            bx + ux * 2,
                            by + uy * 2,
                            bx - px * half,
                            by - py * half,
                        ),
                        PathCommand.Close,
                    ),
                fill = paint,
                stroke = paint,
                width = 1.0,
            )
        )
    }

    if (head == EdgeHead.CIRCLE) {
        return listOf(
            PathItem(
                d = ellipse(tx - ux * 4.5, ty - uy * 4.5, 4.0, 4.0),
                fill = tone("canvas"),
                stroke = paint,
                width = 1.5,
            )
        )
    }

    if (head == EdgeHead.CROSS) {
        val cx = tx - ux * 5
        val cy = ty - uy * 5
        val s = 4.0
        return listOf(
            PathItem(
                d =
                    listOf(
                        PathCommand.Move(cx - (ux + px) * s, cy - (uy + py) * s),
                        PathCommand.Line(cx + (ux + px) * s, cy + (uy + py) * s),
                        PathCommand.Move(cx - (ux - px) * s, cy - (uy - py) * s),
                        PathCommand.Line(cx + (ux - px) * s, cy + (uy - py) * s),
                    ),
                stroke = paint,
                width = 1.75,
            )
        )
    }

    return emptyList()
}

/** Space reserved between an edge's end and its head. */
private fun headInset(head: EdgeHead) =
    when (head) {
        EdgeHead.ARROW -> ARROW
        EdgeHead.CIRCLE -> 8.5
        EdgeHead.CROSS -> 1.0
        EdgeHead.NONE -> 0.0
    }

// ---------------------------------------------------------------------------
// Layered graph layout (flowcharts and state diagrams).

private enum class LKind {
    NODE,
    DUMMY,
    LABEL,
    LEFT,
    RIGHT,
}

private class LNode(
    val key: String,
    val kind: LKind,
    val w: Double,
    val h: Double,
    /** Innermost containing cluster. */
    val cluster: String?,
    var rank: Int = 0,
    val node: GraphNode? = null,
    val box: NodeBox? = null,
) {
    var order = 0
    var x = 0.0
    var y = 0.0
    /** Extra room on the right for self loops. */
    var extra = 0.0
    /** Extra space along the rank axis for tall/wide loop labels, without moving shape ports. */
    var rankPadding = 0.0
}

private class LEdge(
    val edge: GraphEdge,
    val index: Int,
    val u: String,
    val v: String,
    val reversed: Boolean,
    val label: TextBlock?,
) {
    var chain: List<LNode> = emptyList()
}

private class Box(val x: Double, val y: Double, val w: Double, val h: Double)

private class Constraint(var u: String, var v: String, val minlen: Int)

private class Link(val edge: GraphEdge, val index: Int, val label: TextBlock?) {
    var back = false
}

private class SelfLoop(
    val edge: GraphEdge,
    val node: LNode,
    val label: TextBlock?,
    val reach: Double,
)

private class Neighbors {
    val up = mutableListOf<LNode>()
    val down = mutableListOf<LNode>()
}

private class Borders {
    val left = mutableListOf<LNode>()
    val right = mutableListOf<LNode>()
}

private class Entry(val key: Double, val nodes: List<LNode>, val cluster: String? = null)

private class Block(var weight: Double, var total: Double, var count: Int)

private fun <K, V> MutableMap<K, MutableList<V>>.append(key: K, value: V) {
    getOrPut(key) { mutableListOf() }.add(value)
}

/** Iterative depth-first search: links reaching a node still on the stack are back links. */
private fun <T> backLinks(
    starts: Iterable<String>,
    from: Map<String, List<T>>,
    target: (T) -> String,
): List<T> {
    val state = HashMap<String, Int>()
    val back = mutableListOf<T>()
    for (start in starts) {
        if (state.containsKey(start)) continue
        val stack = mutableListOf(start to IntArray(1))
        state[start] = 1
        while (stack.isNotEmpty()) {
            val (key, position) = stack.last()
            val list = from[key].orEmpty()
            if (position[0] >= list.size) {
                state[key] = 2
                stack.removeAt(stack.size - 1)
                continue
            }

            val link = list[position[0]++]
            val next = target(link)
            val seen = state[next]
            if (seen == 1) {
                back.add(link)
            } else if (seen == null) {
                state[next] = 1
                stack.add(next to IntArray(1))
            }
        }
    }

    return back
}

/** Include every drawn primitive, including loop labels that extend beyond ranked nodes. */
private fun fitScene(scene: Scene, measure: TextMeasure): Scene {
    var left = 0.0
    var top = 0.0
    var right = scene.width
    var bottom = scene.height
    fun include(x: Double, y: Double) {
        if (x < 0) left = min(left, x - MARGIN)
        if (y < 0) top = min(top, y - MARGIN)
        if (x > scene.width) right = max(right, x + MARGIN)
        if (y > scene.height) bottom = max(bottom, y + MARGIN)
    }
    for (item in scene.items) {
        when (item) {
            is PathItem -> {
                val stroke = (item.width ?: 0.0) / 2
                fun point(x: Double, y: Double) {
                    include(x - stroke, y - stroke)
                    include(x + stroke, y + stroke)
                }
                for (command in item.d) {
                    when (command) {
                        is PathCommand.Move -> point(command.x, command.y)
                        is PathCommand.Line -> point(command.x, command.y)
                        is PathCommand.Cubic -> {
                            point(command.x1, command.y1)
                            point(command.x2, command.y2)
                            point(command.x, command.y)
                        }
                        PathCommand.Close -> Unit
                    }
                }
            }
            is TextItem -> {
                val width =
                    item.lines.maxOfOrNull { measure.width(it, item.size, item.weight) } ?: 0.0
                val x =
                    item.x -
                        when (item.anchor) {
                            TextAnchor.MIDDLE -> width / 2
                            TextAnchor.END -> width
                            TextAnchor.START -> 0.0
                        }
                val height = item.lines.size * lineHeight(item.size)
                include(x, item.y - height / 2)
                include(x + width, item.y + height / 2)
            }
        }
    }
    if (left == 0.0 && top == 0.0 && right == scene.width && bottom == scene.height) return scene
    val items =
        scene.items.map { item ->
            when (item) {
                is TextItem -> item.copy(x = item.x - left, y = item.y - top)
                is PathItem ->
                    item.copy(
                        d =
                            item.d.map { command ->
                                when (command) {
                                    is PathCommand.Move ->
                                        PathCommand.Move(command.x - left, command.y - top)
                                    is PathCommand.Line ->
                                        PathCommand.Line(command.x - left, command.y - top)
                                    is PathCommand.Cubic ->
                                        PathCommand.Cubic(
                                            command.x1 - left,
                                            command.y1 - top,
                                            command.x2 - left,
                                            command.y2 - top,
                                            command.x - left,
                                            command.y - top,
                                        )
                                    PathCommand.Close -> PathCommand.Close
                                }
                            }
                    )
            }
        }
    return Scene(ceil(right - left), ceil(bottom - top), items, scene.label)
}

internal fun layoutGraph(graph: GraphDiagram, measure: TextMeasure): Scene {
    if (graph.nodes.isEmpty() && graph.clusters.isEmpty())
        throw DiagramError("This diagram has nothing to draw.")
    val horizontal = graph.direction == Direction.LR || graph.direction == Direction.RL
    val annotations = LinkedHashMap<String, MutableList<GraphNode>>()
    val annotationBoxes = HashMap<String, NodeBox>()
    for (note in graph.nodes) {
        val annotation = note.annotation ?: continue
        val target = graph.nodes.find { it.id == annotation.target && it.annotation == null }
        if (target == null || graph.edges.any { it.from == target.id && it.to == target.id })
            throw DiagramError(
                "Notes on composite states or states with self transitions are not supported."
            )
        annotations.getOrPut(target.id) { mutableListOf() }.add(note)
        annotationBoxes[note.id] = nodeBox(note, horizontal, measure)
    }
    val clusters = LinkedHashMap<String, GraphCluster>()
    for (cluster in graph.clusters) clusters[cluster.id] = cluster
    val chains = HashMap<String, List<String>>()
    fun chainOf(id: String?): List<String> {
        if (id.isNullOrEmpty()) return emptyList()
        return chains.getOrPut(id) {
            val chain = ArrayDeque<String>()
            var current: String? = id
            while (!current.isNullOrEmpty() && clusters.containsKey(current)) {
                chain.addFirst(current)
                current = clusters.getValue(current).parent
            }
            chain.toList()
        }
    }

    // Layout nodes, in layout space: x across ranks, y along them.
    val nodes = LinkedHashMap<String, LNode>()
    fun add(node: LNode): LNode {
        if (!nodes.containsKey(node.key) && nodes.size >= MAX_LAYOUT_NODES)
            throw DiagramError("This diagram is too large to draw.")
        nodes[node.key] = node
        return node
    }

    for (node in graph.nodes) {
        if (node.annotation != null) continue
        val box = nodeBox(node, horizontal, measure)
        val notes = annotations[node.id].orEmpty()
        fun sideHeight(side: NotePlacement) =
            notes
                .filter { it.annotation!!.side == side }
                .sumOf { annotationBoxes.getValue(it.id).h + 12 } - 12
        val noteWidth = notes.maxOfOrNull { annotationBoxes.getValue(it.id).w + 28 } ?: 0.0
        val reservedWidth = box.w + noteWidth * 2
        val reservedHeight =
            max(box.h, max(sideHeight(NotePlacement.LEFT), sideHeight(NotePlacement.RIGHT)))
        add(
            LNode(
                key = node.id,
                kind = LKind.NODE,
                w = if (horizontal) reservedHeight else reservedWidth,
                h = if (horizontal) reservedWidth else reservedHeight,
                cluster = node.parent,
                node = node,
                box = box,
            )
        )
    }

    val titles = HashMap<String, TextBlock>()
    for (cluster in graph.clusters) titles[cluster.id] =
        textBlock(cluster.label, CLUSTER_FONT, 600, 240.0, measure)

    // An empty cluster still draws its box around a placeholder.
    val members = LinkedHashMap<String, MutableList<String>>()
    for (node in graph.nodes) {
        if (node.annotation != null) continue
        for (id in chainOf(node.parent)) members.append(id, node.id)
    }

    for (cluster in graph.clusters) {
        if (members.containsKey(cluster.id)) continue
        val key = "\u0000empty:${cluster.id}"
        val title = titles.getValue(cluster.id)
        add(
            LNode(
                key = key,
                kind = LKind.DUMMY,
                w = if (horizontal) 24.0 else max(title.width, 48.0),
                h = 24.0,
                cluster = cluster.id,
            )
        )
        for (id in chainOf(cluster.id)) members.append(id, key)
    }

    // Rank constraints. A link to or from a subgraph constrains every member, so the whole
    // subgraph sits before or after the other end, as its box will be drawn.
    val constraints = mutableListOf<Constraint>()
    fun ends(id: String): List<String> =
        if (clusters.containsKey(id)) members.getValue(id) else listOf(id)
    val links = mutableListOf<Link>()
    val loops = mutableListOf<SelfLoop>()
    val nextLoopReach = mutableMapOf<String, Double>()
    graph.edges.forEachIndexed { index, edge ->
        val label =
            if (edge.label.isNotEmpty()) textBlock(edge.label, LABEL_FONT, 500, 160.0, measure)
            else null
        if (edge.from == edge.to) {
            val node = nodes[edge.from]
            if (node != null) {
                val reach = nextLoopReach[node.key] ?: (24 + min(node.h, 40.0) / 2)
                val labelExtent =
                    if (label != null) (if (horizontal) label.height else label.width) else 0.0
                val rankExtent =
                    if (label != null) (if (horizontal) label.width else label.height) else 0.0
                nextLoopReach[node.key] = reach + max(32.0, (labelExtent + 16) / 0.75)
                node.extra = max(node.extra, max(reach + 12, reach * 0.75 + labelExtent + 18))
                node.rankPadding = max(node.rankPadding, (rankExtent - node.h) / 2 + 6)
                loops.add(SelfLoop(edge, node, label, reach))
            }

            return@forEachIndexed
        }

        links.add(Link(edge, index, label))
    }

    // Break cycles twice: first between links as written, with subgraphs as single ends, so a
    // link back into a subgraph is the one reversed; then between the members it expands to.
    val linksFrom = LinkedHashMap<String, MutableList<Link>>()
    for (link in links) linksFrom.append(link.edge.from, link)
    for (link in
        backLinks(graph.nodes.map { it.id } + clusters.keys, linksFrom) { it.edge.to }) link.back =
        true

    for (link in links) {
        val from = if (link.back) link.edge.to else link.edge.from
        val to = if (link.back) link.edge.from else link.edge.to
        for (u in ends(from)) {
            for (v in ends(to)) {
                if (u != v) {
                    if (constraints.size >= MAX_LAYOUT_CONSTRAINTS)
                        throw DiagramError("This diagram has too many expanded links to draw.")
                    constraints.add(Constraint(u, v, link.edge.length * 2))
                }
            }
        }
    }

    val outgoing = LinkedHashMap<String, MutableList<Constraint>>()
    for (constraint in constraints) outgoing.append(constraint.u, constraint)
    val backward = LinkedHashSet(backLinks(nodes.keys.toList(), outgoing) { it.v })
    for (constraint in backward) {
        val u = constraint.u
        constraint.u = constraint.v
        constraint.v = u
    }

    // Rank by longest path, then pull nodes with more outgoing than incoming constraints down.
    val ins = HashMap<String, MutableList<Constraint>>()
    val outs = HashMap<String, MutableList<Constraint>>()
    for (constraint in constraints) {
        ins.append(constraint.v, constraint)
        outs.append(constraint.u, constraint)
    }

    val indegree = HashMap<String, Int>()
    for (key in nodes.keys) indegree[key] = ins[key]?.size ?: 0
    val queue = ArrayDeque(nodes.keys.filter { indegree[it] == 0 })
    val topo = mutableListOf<String>()
    while (queue.isNotEmpty()) {
        val key = queue.removeFirst()
        topo.add(key)
        for (constraint in outs[key].orEmpty()) {
            val next = nodes.getValue(constraint.v)
            next.rank = max(next.rank, nodes.getValue(key).rank + constraint.minlen)
            indegree[constraint.v] = indegree.getValue(constraint.v) - 1
            if (indegree[constraint.v] == 0) queue.addLast(constraint.v)
        }
    }

    for (key in topo.asReversed()) {
        val out = outs[key].orEmpty()
        if (out.isEmpty() || out.size <= (ins[key]?.size ?: 0)) continue
        val node = nodes.getValue(key)
        node.rank = out.minOf { nodes.getValue(it.v).rank - it.minlen }
    }

    val lowest = nodes.values.minOfOrNull { it.rank } ?: 0
    for (node in nodes.values) node.rank -= lowest

    // With ranks known, a subgraph's link attaches to its member nearest the other end.
    fun nearest(id: String, rank: Double): String {
        if (!clusters.containsKey(id)) return id
        var best = members.getValue(id)[0]
        for (key in members.getValue(id)) {
            if (abs(nodes.getValue(key).rank - rank) < abs(nodes.getValue(best).rank - rank))
                best = key
        }

        return best
    }

    val edges = mutableListOf<LEdge>()
    val flats = mutableListOf<LEdge>()
    for (link in links) {
        val edge = link.edge
        fun middle(id: String): Double {
            val ranks = ends(id).map { nodes.getValue(it).rank }
            return ranks.fold(0.0) { sum, rank -> sum + rank } / ranks.size
        }

        val u = nearest(edge.from, middle(edge.to))
        val v = nearest(edge.to, nodes.getValue(u).rank.toDouble())
        if (u == v) continue
        val reversed = nodes.getValue(u).rank > nodes.getValue(v).rank
        val layout =
            LEdge(
                edge = edge,
                index = link.index,
                u = if (reversed) v else u,
                v = if (reversed) u else v,
                reversed = reversed,
                label = link.label,
            )
        if (nodes.getValue(u).rank == nodes.getValue(v).rank) flats.add(layout)
        else edges.add(layout)
    }

    // Each edge becomes a chain of dummies, one per intermediate rank; labels take the middle one.
    fun lca(a: String?, b: String?): String? {
        val left = chainOf(a)
        val right = chainOf(b)
        var common: String? = null
        var index = 0
        while (index < min(left.size, right.size) && left[index] == right[index]) {
            common = left[index]
            index++
        }
        return common
    }

    for (edge in edges) {
        val u = nodes.getValue(edge.u)
        val v = nodes.getValue(edge.v)
        val cluster = lca(u.cluster, v.cluster)
        val middle = u.rank + (v.rank - u.rank) / 2
        val chain = mutableListOf(u)
        for (rank in u.rank + 1 until v.rank) {
            val label = edge.label
            val labelled = label != null && rank == middle
            chain.add(
                add(
                    LNode(
                        key = "\u0000edge:${edge.index}:$rank",
                        kind = if (labelled) LKind.LABEL else LKind.DUMMY,
                        w =
                            if (labelled) (if (horizontal) label.height + 8 else label.width + 14)
                            else 0.0,
                        h =
                            if (labelled) (if (horizontal) label.width + 14 else label.height + 8)
                            else 0.0,
                        cluster = cluster,
                        rank = rank,
                    )
                )
            )
        }

        chain.add(v)
        edge.chain = chain
    }

    // Clusters keep a left and right border node on every rank they span.
    val spans = LinkedHashMap<String, IntArray>()
    for (node in nodes.values) {
        for (id in chainOf(node.cluster)) {
            val span = spans[id]
            spans[id] =
                if (span != null) intArrayOf(min(span[0], node.rank), max(span[1], node.rank))
                else intArrayOf(node.rank, node.rank)
        }
    }

    for ((id, span) in spans) {
        for (rank in span[0]..span[1]) {
            for (kind in listOf(LKind.LEFT, LKind.RIGHT)) {
                val name = if (kind == LKind.LEFT) "left" else "right"
                add(
                    LNode(
                        key = "\u0000$name:$id:$rank",
                        kind = kind,
                        w = 0.0,
                        h = 0.0,
                        cluster = id,
                        rank = rank,
                    )
                )
            }
        }
    }

    val rankCount = max(0, nodes.values.maxOfOrNull { it.rank } ?: 0) + 1
    var layers: MutableList<List<LNode>> = MutableList(rankCount) { emptyList() }
    val building = List(rankCount) { mutableListOf<LNode>() }
    val neighbors = HashMap<String, Neighbors>()
    for (node in nodes.values) neighbors[node.key] = Neighbors()
    for (edge in edges) {
        for (index in 1 until edge.chain.size) {
            neighbors.getValue(edge.chain[index - 1].key).down.add(edge.chain[index])
            neighbors.getValue(edge.chain[index].key).up.add(edge.chain[index - 1])
        }
    }

    // Initial order: depth-first from the declared nodes, so authored order mostly survives.
    val visited = HashSet<String>()
    fun visit(start: LNode) {
        val stack = mutableListOf(start)
        while (stack.isNotEmpty()) {
            val node = stack.removeAt(stack.size - 1)
            if (!visited.add(node.key)) continue
            building[node.rank].add(node)
            stack.addAll(neighbors.getValue(node.key).down.asReversed())
        }
    }

    for (node in nodes.values) {
        if (node.kind == LKind.NODE || node.kind == LKind.DUMMY) visit(node)
    }

    for (node in nodes.values) {
        if (!visited.contains(node.key)) visit(node)
    }

    for (rank in 0 until rankCount) layers[rank] = building[rank]

    fun clusterDepth(id: String?) = chainOf(id).size
    val siblingOrder = HashMap<String, Double>()

    /** Sort one layer by key, keeping each cluster contiguous and bordered. */
    fun arrange(layer: List<LNode>, key: (LNode) -> Double): List<LNode> {
        fun place(items: List<LNode>, depth: Int, inside: String?): List<LNode> {
            val entries = mutableListOf<Entry>()
            val groups = LinkedHashMap<String, MutableList<LNode>>()
            var left: LNode? = null
            var right: LNode? = null
            for (node in items) {
                val chain = chainOf(node.cluster)
                if (chain.size == depth) {
                    if (node.kind == LKind.LEFT && node.cluster == inside) left = node
                    else if (node.kind == LKind.RIGHT && node.cluster == inside) right = node
                    else entries.add(Entry(key(node), listOf(node)))
                } else {
                    groups.append(chain[depth], node)
                }
            }

            for ((cluster, group) in groups) {
                val keyed = group.filter { it.kind != LKind.LEFT && it.kind != LKind.RIGHT }
                val value =
                    if (keyed.isNotEmpty())
                        keyed.fold(0.0) { sum, node -> sum + key(node) } / keyed.size
                    else key(group[0])
                entries.add(Entry(value, place(group, depth + 1, cluster), cluster))
            }

            entries.sortWith { a, b -> sortOrder(a.key - b.key) }
            // Sibling clusters keep one relative order on every rank, or their boxes would overlap.
            val slots = entries.indices.filter { entries[it].cluster != null }
            val ordered =
                slots
                    .map { entries[it] }
                    .sortedWith { a, b ->
                        sortOrder(
                            (siblingOrder[a.cluster!!] ?: 0.0) - (siblingOrder[b.cluster!!] ?: 0.0)
                        )
                    }
            slots.forEachIndexed { index, slot -> entries[slot] = ordered[index] }
            return listOfNotNull(left) + entries.flatMap { it.nodes } + listOfNotNull(right)
        }

        val sorted = place(layer, 0, null)
        sorted.forEachIndexed { index, node -> node.order = index }
        return sorted
    }

    fun updateSiblingOrder() {
        val totals = LinkedHashMap<String, DoubleArray>()
        for (layer in layers) {
            for (node in layer) {
                if (node.cluster.isNullOrEmpty()) continue
                for (id in chainOf(node.cluster)) {
                    val total = totals[id] ?: doubleArrayOf(0.0, 0.0)
                    totals[id] =
                        doubleArrayOf(
                            total[0] + node.order.toDouble() / max(1, layer.size - 1),
                            total[1] + 1,
                        )
                }
            }
        }

        for ((id, total) in totals) siblingOrder[id] = total[0] / total[1]
    }

    // Crossings between two layers are inversions of the lower ends, counted with a Fenwick tree.
    fun crossings(): Int {
        var count = 0
        for (rank in 0 until layers.size - 1) {
            val ends = mutableListOf<Int>()
            for (node in layers[rank]) {
                ends.addAll(neighbors.getValue(node.key).down.map { it.order }.sorted())
            }

            val size = layers[rank + 1].size + 1
            val tree = IntArray(size + 1)
            for ((index, end) in ends.withIndex()) {
                var below = 0
                var at = end + 1
                while (at > 0) {
                    below += tree[at]
                    at -= at and -at
                }
                count += index - below
                at = end + 1
                while (at <= size) {
                    tree[at]++
                    at += at and -at
                }
            }
        }

        return count
    }

    for (layer in layers) layer.forEachIndexed { index, node -> node.order = index }

    updateSiblingOrder()
    for (rank in layers.indices) layers[rank] = arrange(layers[rank]) { it.order.toDouble() }

    var best = layers.map { it.toList() }
    var bestCrossings = crossings()
    fun barycenter(node: LNode, up: Boolean): Double {
        val list = neighbors.getValue(node.key).let { if (up) it.up else it.down }
        if (list.isEmpty()) return node.order.toDouble()
        return list.fold(0.0) { sum, next -> sum + next.order } / list.size
    }

    var iteration = 0
    while (iteration < 16 && bestCrossings > 0) {
        val down = iteration % 2 == 0
        updateSiblingOrder()
        val ranks = layers.indices.toList()
        for (rank in if (down) ranks.drop(1) else ranks.asReversed().drop(1)) layers[rank] =
            arrange(layers[rank]) { barycenter(it, up = down) }
        val count = crossings()
        if (count < bestCrossings) {
            bestCrossings = count
            best = layers.map { it.toList() }
        }
        iteration++
    }

    for ((rank, layer) in best.withIndex()) {
        layers[rank] = layer
        layer.forEachIndexed { index, node -> node.order = index }
    }

    // Horizontal positions. Separation depends on what sits side by side.
    fun separation(a: LNode, b: LNode): Double {
        val half = (a.w + b.w) / 2 + a.extra
        if (a.kind == LKind.LEFT && b.cluster == a.cluster && b.kind != LKind.RIGHT)
            return half +
                CLUSTER_PAD +
                (if (horizontal) titles.getValue(a.cluster!!).height + 6 else 0.0)
        if (a.kind == LKind.LEFT || b.kind == LKind.RIGHT) return half + CLUSTER_PAD
        if (a.kind == LKind.RIGHT || b.kind == LKind.LEFT)
            return half + (if (a.kind == LKind.RIGHT && b.kind == LKind.LEFT) 14 else 20)
        if (a.kind == LKind.DUMMY || b.kind == LKind.DUMMY) return half + EDGE_SEP
        return half + NODE_SEP
    }

    fun weight(a: LNode, b: LNode): Double {
        val dummyA = a.kind == LKind.DUMMY || a.kind == LKind.LABEL
        val dummyB = b.kind == LKind.DUMMY || b.kind == LKind.LABEL
        return if (dummyA && dummyB) 8.0 else if (dummyA || dummyB) 2.0 else 1.0
    }

    for (layer in layers) {
        for (index in 1 until layer.size) {
            val least = layer[index - 1].x + separation(layer[index - 1], layer[index])
            if (layer[index].x < least) layer[index].x = least
        }
    }

    /** Pool-adjacent-violators: closest positions to the targets that keep the separations. */
    fun solve(layer: List<LNode>, targets: List<Double>, weights: List<Double>) {
        val offsets = mutableListOf(0.0)
        for (index in 1 until layer.size) offsets.add(
            offsets[index - 1] + separation(layer[index - 1], layer[index])
        )
        val blocks = mutableListOf<Block>()
        for (index in layer.indices) {
            blocks.add(Block(weights[index], weights[index] * (targets[index] - offsets[index]), 1))
            while (blocks.size > 1) {
                val last = blocks[blocks.size - 1]
                val previous = blocks[blocks.size - 2]
                if (previous.total / previous.weight <= last.total / last.weight) break
                blocks.removeAt(blocks.size - 1)
                previous.weight += last.weight
                previous.total += last.total
                previous.count += last.count
            }
        }

        var index = 0
        for (block in blocks) {
            val value = block.total / block.weight
            repeat(block.count) {
                layer[index].x = value + offsets[index]
                index++
            }
        }
    }

    for (pass in 0 until 10) {
        val down = pass % 2 == 0
        val ranks = layers.indices.toList()
        for (rank in if (down) ranks else ranks.asReversed()) {
            val layer = layers[rank]
            val targets = mutableListOf<Double>()
            val weights = mutableListOf<Double>()
            for (node in layer) {
                val around = neighbors.getValue(node.key)
                val primary = if (down) around.up else around.down
                val list =
                    if (primary.isNotEmpty()) primary else if (down) around.down else around.up
                if (list.isEmpty()) {
                    targets.add(node.x)
                    weights.add(
                        if (node.kind == LKind.LEFT || node.kind == LKind.RIGHT) 0.25 else 0.5
                    )
                    continue
                }

                var total = 0.0
                var sum = 0.0
                for (next in list) {
                    val w = weight(node, next)
                    total += w
                    sum += w * next.x
                }

                targets.add(sum / total)
                weights.add(total)
            }

            solve(layer, targets, weights)
        }
    }

    // Align every cluster's borders across ranks by moving them outward only: left borders to
    // their leftmost rank, right borders to their rightmost. Whatever lies outside is pushed
    // away, so members never move and the passes settle.
    val borders = LinkedHashMap<String, Borders>()
    for (node in nodes.values) {
        if (node.kind != LKind.LEFT && node.kind != LKind.RIGHT) continue
        val entry = borders.getOrPut(node.cluster!!) { Borders() }
        if (node.kind == LKind.LEFT) entry.left.add(node) else entry.right.add(node)
    }

    fun minimumWidth(id: String) =
        if (horizontal) 0.0 else titles.getValue(id).width + CLUSTER_PAD * 2
    for (pass in 0 until 50) {
        var changed = false
        for ((id, entry) in borders) {
            val l = minimum(entry.left.map { it.x })
            val r = maximum(entry.right.map { it.x } + (l + minimumWidth(id)))
            for (node in entry.left + entry.right) {
                val x = if (node.kind == LKind.LEFT) l else r
                if (node.x == x) continue
                node.x = x
                changed = true
                val layer = layers[node.rank]
                if (node.kind == LKind.LEFT) {
                    for (index in node.order - 1 downTo 0) layer[index].x =
                        min(
                            layer[index].x,
                            layer[index + 1].x - separation(layer[index], layer[index + 1]),
                        )
                } else {
                    for (index in node.order + 1 until layer.size) layer[index].x =
                        max(
                            layer[index].x,
                            layer[index - 1].x + separation(layer[index - 1], layer[index]),
                        )
                }
            }
        }

        if (!changed) break
    }

    // Straighten long edges where their layers leave room for a direct line.
    fun fits(node: LNode, x: Double): Boolean {
        val layer = layers[node.rank]
        val left = layer.getOrNull(node.order - 1)
        val right = layer.getOrNull(node.order + 1)
        return (left == null || left.x + separation(left, node) <= x + 0.01) &&
            (right == null || x + separation(node, right) <= right.x + 0.01)
    }

    repeat(2) {
        for (edge in edges) {
            val inner = edge.chain.subList(1, edge.chain.size - 1)
            if (inner.isEmpty()) continue
            val a = edge.chain[0]
            val b = edge.chain.last()
            fun target(node: LNode) = a.x + (b.x - a.x) * (node.rank - a.rank) / (b.rank - a.rank)
            if (inner.all { fits(it, target(it)) }) {
                for (node in inner) node.x = target(node)
            }
        }
    }

    // Center each subgraph's content when its title made the box wider.
    val deepestFirst = graph.clusters.sortedWith { a, b -> clusterDepth(b.id) - clusterDepth(a.id) }
    for (cluster in deepestFirst) {
        val entry = borders[cluster.id] ?: continue
        val inside =
            nodes.values.filter {
                !it.cluster.isNullOrEmpty() &&
                    chainOf(it.cluster).contains(cluster.id) &&
                    !((it.kind == LKind.LEFT || it.kind == LKind.RIGHT) && it.cluster == cluster.id)
            }
        if (inside.isEmpty()) continue
        val contentLeft = minimum(inside.map { it.x - it.w / 2 })
        val contentRight = maximum(inside.map { it.x + it.w / 2 + it.extra })
        val leftRoom =
            contentLeft -
                entry.left[0].x -
                CLUSTER_PAD -
                (if (horizontal) titles.getValue(cluster.id).height + 6 else 0.0)
        val rightRoom = entry.right[0].x - contentRight - CLUSTER_PAD
        var shift = (rightRoom - leftRoom) / 2
        val moving = inside.toSet()
        // Every rank keeps its separation constraints when the whole cluster moves together.
        for (node in inside) {
            val right = layers[node.rank].getOrNull(node.order + 1)
            if (right != null && right !in moving)
                shift = min(shift, right.x - node.x - separation(node, right))
        }
        if (shift > 0.5) {
            for (node in inside) node.x += shift
        }
    }

    // Vertical positions: rank gaps grow where clusters open or close.
    val rankHeight = layers.map { layer ->
        layer.fold(0.0) { tallest, node -> max(tallest, node.h + node.rankPadding * 2) }
    }
    val opening = DoubleArray(rankCount)
    val closing = DoubleArray(rankCount)
    for (layer in layers) {
        for (node in layer) {
            var opens = 0.0
            var closes = 0.0
            for (id in chainOf(node.cluster)) {
                val span = spans.getValue(id)
                if (span[0] == node.rank)
                    opens +=
                        CLUSTER_PAD +
                            (if (graph.direction == Direction.TB) titles.getValue(id).height + 4
                            else 0.0)
                if (span[1] == node.rank)
                    closes +=
                        CLUSTER_PAD +
                            (if (graph.direction == Direction.BT) titles.getValue(id).height + 4
                            else 0.0)
            }

            opening[node.rank] = max(opening[node.rank], opens)
            closing[node.rank] = max(closing[node.rank], closes)
        }
    }

    val rankY = DoubleArray(rankCount)
    var cursor = 0.0
    for (rank in 0 until rankCount) {
        cursor += (if (rank != 0) RANK_GAP else 0.0) + opening[rank]
        rankY[rank] = cursor + rankHeight[rank] / 2
        cursor += rankHeight[rank] + closing[rank]
    }

    for (node in nodes.values) node.y = rankY[node.rank]

    // Notes reserve space with their target, then sit on the requested physical side in every
    // direction.
    for ((target, notes) in annotations) {
        val node = nodes.getValue(target)
        for (side in listOf(NotePlacement.LEFT, NotePlacement.RIGHT)) {
            val attached = notes.filter { it.annotation!!.side == side }
            val total = attached.sumOf { annotationBoxes.getValue(it.id).h + 12 } - 12
            var y = -total / 2
            for (note in attached) {
                val box = annotationBoxes.getValue(note.id)
                val dx =
                    (if (side == NotePlacement.LEFT) -1 else 1) *
                        (node.box!!.w / 2 + 28 + box.w / 2)
                val dy = y + box.h / 2
                val offset =
                    when (graph.direction) {
                        Direction.BT -> Pt(dx, -dy)
                        Direction.LR -> Pt(dy, dx)
                        Direction.RL -> Pt(dy, -dx)
                        else -> Pt(dx, dy)
                    }
                val noteNode =
                    add(
                        LNode(
                            key = note.id,
                            kind = LKind.NODE,
                            w = if (horizontal) box.h else box.w,
                            h = if (horizontal) box.w else box.h,
                            cluster = node.cluster,
                            rank = node.rank,
                            node = note,
                            box = box,
                        )
                    )
                noteNode.x = node.x + offset.x
                noteNode.y = node.y + offset.y
                y += box.h + 12
            }
        }
    }

    // Cluster boxes, innermost first so parents can include their children.
    val boxes = LinkedHashMap<String, Box>()
    for (cluster in deepestFirst) {
        if (!spans.containsKey(cluster.id)) continue
        val entry = borders.getValue(cluster.id)
        var top = Double.POSITIVE_INFINITY
        var bottom = Double.NEGATIVE_INFINITY
        for (node in nodes.values) {
            if (
                node.kind == LKind.LEFT ||
                    node.kind == LKind.RIGHT ||
                    !chainOf(node.cluster).contains(cluster.id)
            )
                continue
            top = min(top, node.y - node.h / 2 - node.rankPadding)
            bottom = max(bottom, node.y + node.h / 2 + node.rankPadding)
        }

        for (child in graph.clusters) {
            val box = if (child.parent == cluster.id) boxes[child.id] else null
            if (box != null) {
                top = min(top, box.y)
                bottom = max(bottom, box.y + box.h)
            }
        }

        val title = titles.getValue(cluster.id).height + 4
        top -= CLUSTER_PAD + (if (graph.direction == Direction.TB) title else 0.0)
        bottom += CLUSTER_PAD + (if (graph.direction == Direction.BT) title else 0.0)
        boxes[cluster.id] =
            Box(entry.left[0].x, top, entry.right[0].x - entry.left[0].x, bottom - top)
    }

    // Layout extents and the transform into the drawing's own direction.
    var minX = Double.POSITIVE_INFINITY
    var maxX = Double.NEGATIVE_INFINITY
    var maxY = 0.0
    for (node in nodes.values) {
        minX = min(minX, node.x - node.w / 2)
        maxX = max(maxX, node.x + node.w / 2 + node.extra)
        maxY = max(maxY, node.y + node.h / 2 + node.rankPadding)
    }

    for (box in boxes.values) {
        minX = min(minX, box.x)
        maxX = max(maxX, box.x + box.w)
        maxY = max(maxY, box.y + box.h)
    }

    val titleBlock = graph.title?.let { textBlock(it, TITLE_FONT, 600, 480.0, measure) }
    val titleSpace = if (titleBlock != null) titleBlock.height + 14 else 0.0
    val extentX = maxX - minX
    val extentY = maxY
    val graphWidth = (if (horizontal) extentY else extentX) + MARGIN * 2
    val titleWidth = if (titleBlock != null) titleBlock.width + MARGIN * 2 else 0.0
    val horizontalMargin = MARGIN + max(0.0, (titleWidth - graphWidth) / 2)
    fun transform(point: Pt): Pt {
        val lx = point.x - minX
        return when (graph.direction) {
            Direction.BT -> Pt(lx + horizontalMargin, extentY - point.y + MARGIN + titleSpace)
            Direction.LR -> Pt(point.y + horizontalMargin, lx + MARGIN + titleSpace)
            Direction.RL -> Pt(extentY - point.y + horizontalMargin, lx + MARGIN + titleSpace)
            Direction.TB -> Pt(lx + horizontalMargin, point.y + MARGIN + titleSpace)
        }
    }

    val width = ceil(max(graphWidth, titleWidth))
    val height = ceil((if (horizontal) extentX else extentY) + MARGIN * 2 + titleSpace)
    fun finalBox(box: Box): Box {
        val a = transform(Pt(box.x, box.y))
        val b = transform(Pt(box.x + box.w, box.y + box.h))
        return Box(min(a.x, b.x), min(a.y, b.y), abs(b.x - a.x), abs(b.y - a.y))
    }

    val items = mutableListOf<SceneItem>()
    val backgrounds = mutableListOf<SceneItem>()
    val edgeItems = mutableListOf<SceneItem>()
    val labelItems = mutableListOf<SceneItem>()
    val titleItems = mutableListOf<SceneItem>()
    val nodeItems = mutableListOf<SceneItem>()

    for ((target, notes) in annotations) {
        val node = nodes.getValue(target)
        val center = transform(Pt(node.x, node.y))
        for (note in notes) {
            val at = nodes.getValue(note.id)
            val position = transform(Pt(at.x, at.y))
            val sign = if (note.annotation!!.side == NotePlacement.LEFT) -1 else 1
            edgeItems.add(
                PathItem(
                    d =
                        listOf(
                            PathCommand.Move(center.x + sign * node.box!!.w / 2, center.y),
                            PathCommand.Line(position.x - sign * at.box!!.w / 2, position.y),
                        ),
                    stroke = DiagramPaint.Tone("edge"),
                    width = 1.0,
                    dash = listOf(4.0, 4.0),
                )
            )
        }
    }

    if (titleBlock != null) {
        items.add(
            TextItem(
                x = width / 2,
                y = MARGIN + titleBlock.height / 2,
                lines = titleBlock.lines,
                size = TITLE_FONT,
                weight = 600,
                anchor = TextAnchor.MIDDLE,
                fill = tone("ink"),
            )
        )
    }

    for (cluster in graph.clusters.sortedWith { a, b -> clusterDepth(a.id) - clusterDepth(b.id) }) {
        val box = boxes[cluster.id] ?: continue
        val area = finalBox(box)
        val style = cluster.style
        val fill = style?.fill?.let { DiagramPaint.Literal(it) } ?: tone("cluster")
        backgrounds.add(
            PathItem(
                d = roundedRect(area.x, area.y, area.w, area.h, 14.0),
                fill = fill,
                stroke = style?.stroke?.let { DiagramPaint.Literal(it) } ?: tone("cluster-line"),
                width = style?.strokeWidth ?: 1.0,
                dash = if (style?.dashed == true) listOf(5.0, 4.0) else null,
            )
        )
        // Titles sit above edges, on a patch of the box's own fill, so lines never cross them.
        val title = titles.getValue(cluster.id)
        if (title.lines.isNotEmpty()) {
            val x = area.x + CLUSTER_PAD - 2
            val y =
                if (graph.direction == Direction.BT) area.y + area.h - 10 - title.height / 2
                else area.y + 10 + title.height / 2
            titleItems.add(
                PathItem(
                    d =
                        roundedRect(
                            x - 4,
                            y - title.height / 2 - 1,
                            title.width + 8,
                            title.height + 2,
                            5.0,
                        ),
                    fill = fill,
                )
            )
            titleItems.add(
                TextItem(
                    x = x,
                    y = y,
                    lines = title.lines,
                    size = CLUSTER_FONT,
                    weight = 600,
                    anchor = TextAnchor.START,
                    fill = textOn(style, "cluster-ink"),
                )
            )
        }
    }

    // Edges are cubic segments with tangents along the rank axis, joined at chain points.
    fun axis(sign: Double): Pt =
        when (graph.direction) {
            Direction.BT -> Pt(0.0, -sign)
            Direction.LR -> Pt(sign, 0.0)
            Direction.RL -> Pt(-sign, 0.0)
            Direction.TB -> Pt(0.0, sign)
        }

    fun edgePaint(edge: GraphEdge): DiagramPaint =
        edge.style?.stroke?.let { DiagramPaint.Literal(it) } ?: tone("edge")
    fun edgeWidth(edge: GraphEdge) =
        edge.style?.strokeWidth ?: (if (edge.line == EdgeLine.THICK) 3.0 else 1.5)

    for (edge in edges) {
        val source = edge.edge
        if (source.line == EdgeLine.INVISIBLE) continue
        val chain = edge.chain
        val first = chain[0]
        val last = chain.last()
        fun port(node: LNode, toward: Double, side: Int): Pt {
            val shape = node.node?.shape ?: return Pt(node.x, node.y)
            val w = node.box?.let { if (horizontal) it.h else it.w } ?: node.w
            val h = node.box?.let { if (horizontal) it.w else it.h } ?: node.h
            val round =
                shape == NodeShape.CIRCLE ||
                    shape == NodeShape.DOUBLE_CIRCLE ||
                    shape == NodeShape.DIAMOND ||
                    shape == NodeShape.CHOICE ||
                    shape == NodeShape.START ||
                    shape == NodeShape.END
            val reach = max(0.0, if (round) w * 0.2 else w / 2 - 12)
            val dx = max(-reach, min(reach, toward - node.x))
            // Transposed outlines only match for symmetric shapes; others leave from their flat
            // side.
            val depth = if (horizontal && !round) h / 2 else outline(shape, w, h, dx)
            return Pt(node.x + dx, node.y + side * depth)
        }

        val startPoint = port(first, (chain.getOrNull(1) ?: last).x, 1)
        val endPoint = port(last, (chain.getOrNull(chain.size - 2) ?: first).x, -1)
        var points =
            listOf(startPoint) + chain.subList(1, chain.size - 1).map { Pt(it.x, it.y) } + endPoint

        // Links to a subgraph stop at its border.
        val fromCluster = if (edge.reversed) source.to else source.from
        val toCluster = if (edge.reversed) source.from else source.to
        if (clusters.containsKey(toCluster) && boxes.containsKey(toCluster)) {
            val box = boxes.getValue(toCluster)
            val outside = points.filter { it.y < box.y }
            val previous = outside.lastOrNull() ?: points[0]
            points = outside + Pt(max(box.x + 12, min(box.x + box.w - 12, previous.x)), box.y)
        }

        if (clusters.containsKey(fromCluster) && boxes.containsKey(fromCluster)) {
            val box = boxes.getValue(fromCluster)
            val outside = points.filter { it.y > box.y + box.h }
            val next = outside.firstOrNull() ?: points.last()
            points =
                listOf(Pt(max(box.x + 12, min(box.x + box.w - 12, next.x)), box.y + box.h)) +
                    outside
        }

        if (points.size < 2) continue
        val startHead = if (edge.reversed) source.end else source.start
        val endHead = if (edge.reversed) source.start else source.end
        // Pull the curve back so heads sit at the outline instead of under the stroke.
        val startInset = headInset(startHead)
        val endInset = headInset(endHead)
        val tipStart = points[0]
        val tipEnd = points.last()
        points =
            points.toMutableList().also {
                it[0] = Pt(tipStart.x, tipStart.y + startInset)
                it[it.size - 1] = Pt(tipEnd.x, tipEnd.y - endInset)
            }

        val d = smoothPath(points, ::transform)
        val paint = edgePaint(source)
        val strokeWidth = edgeWidth(source)
        val dashed = source.line == EdgeLine.DOTTED || source.style?.dashed == true
        edgeItems.add(
            PathItem(
                d = d,
                stroke = paint,
                width = strokeWidth,
                dash = if (dashed) listOf(4.0, 4.0) else null,
            )
        )
        edgeItems.addAll(headPaths(endHead, transform(tipEnd), axis(1.0), paint, strokeWidth))
        edgeItems.addAll(headPaths(startHead, transform(tipStart), axis(-1.0), paint, strokeWidth))

        val labelNode = chain.find { it.kind == LKind.LABEL }
        if (edge.label != null) {
            val middle = points[points.size / 2]
            val before = points.getOrNull(points.size / 2 - 1) ?: middle
            val at =
                if (labelNode != null) Pt(labelNode.x, labelNode.y)
                else Pt((middle.x + before.x) / 2, (middle.y + before.y) / 2)
            labelItems.addAll(edgeLabel(edge.label, transform(at)))
        }
    }

    // Links between nodes of one rank run straight across it.
    for (edge in flats) {
        val source = edge.edge
        if (source.line == EdgeLine.INVISIBLE) continue
        val pair =
            listOf(nodes.getValue(edge.u), nodes.getValue(edge.v)).sortedWith { left, right ->
                sortOrder(left.x - right.x)
            }
        val a = pair[0]
        val b = pair[1]
        val forward = a.key == (if (edge.reversed) edge.v else edge.u)
        val startHead = if (forward) source.start else source.end
        val endHead = if (forward) source.end else source.start
        val tipStart = Pt(a.x + a.w / 2 + a.extra, a.y)
        val tipEnd = Pt(b.x - b.w / 2, b.y)
        val from = Pt(tipStart.x + headInset(startHead), a.y)
        val to = Pt(tipEnd.x - headInset(endHead), b.y)
        val third = (to.x - from.x) / 3
        val paint = edgePaint(source)
        val strokeWidth = edgeWidth(source)
        val origin = transform(Pt(0.0, 0.0))
        val unit = transform(Pt(1.0, 0.0))
        val across = Pt(unit.x - origin.x, unit.y - origin.y)
        val start = transform(from)
        val control1 = transform(Pt(from.x + third, from.y))
        val control2 = transform(Pt(to.x - third, to.y))
        val end = transform(to)
        edgeItems.add(
            PathItem(
                d =
                    listOf(
                        PathCommand.Move(start.x, start.y),
                        PathCommand.Cubic(
                            control1.x,
                            control1.y,
                            control2.x,
                            control2.y,
                            end.x,
                            end.y,
                        ),
                    ),
                stroke = paint,
                width = strokeWidth,
                dash =
                    if (source.line == EdgeLine.DOTTED || source.style?.dashed == true)
                        listOf(4.0, 4.0)
                    else null,
            )
        )
        edgeItems.addAll(headPaths(endHead, transform(tipEnd), across, paint, strokeWidth))
        edgeItems.addAll(
            headPaths(startHead, transform(tipStart), Pt(-across.x, -across.y), paint, strokeWidth)
        )
        if (edge.label != null)
            labelItems.addAll(
                edgeLabel(
                    edge.label,
                    transform(Pt((from.x + to.x) / 2, a.y - edge.label.height / 2 - 6)),
                )
            )
    }

    for (loop in loops) {
        val node = loop.node
        val edge = loop.edge
        val right = node.x + node.w / 2
        val reach = loop.reach
        val bend = (24 + min(node.h, 40.0) / 2) * 0.7
        val top = node.y - min(node.h / 4, 10.0)
        val bottom = node.y + min(node.h / 4, 10.0)
        val inset = headInset(edge.end)
        val start = transform(Pt(right + headInset(edge.start), top))
        val control1 = transform(Pt(right + reach, top - bend))
        val control2 = transform(Pt(right + reach, bottom + bend))
        val end = transform(Pt(right + inset, bottom))
        val d =
            listOf(
                PathCommand.Move(start.x, start.y),
                PathCommand.Cubic(control1.x, control1.y, control2.x, control2.y, end.x, end.y),
            )
        val paint = edgePaint(edge)
        val strokeWidth = edgeWidth(edge)
        if (edge.line != EdgeLine.INVISIBLE) {
            edgeItems.add(
                PathItem(
                    d = d,
                    stroke = paint,
                    width = strokeWidth,
                    dash =
                        if (edge.line == EdgeLine.DOTTED || edge.style?.dashed == true)
                            listOf(4.0, 4.0)
                        else null,
                )
            )
            val origin = transform(Pt(0.0, 0.0))
            val back = transform(Pt(-1.0, 0.0))
            edgeItems.addAll(
                headPaths(
                    edge.end,
                    transform(Pt(right, bottom)),
                    Pt(back.x - origin.x, back.y - origin.y),
                    paint,
                    strokeWidth,
                )
            )
            edgeItems.addAll(
                headPaths(
                    edge.start,
                    transform(Pt(right, top)),
                    Pt(back.x - origin.x, back.y - origin.y),
                    paint,
                    strokeWidth,
                )
            )
            if (loop.label != null) {
                val at =
                    transform(
                        Pt(
                            right +
                                reach * 0.75 +
                                (if (horizontal) loop.label.height else loop.label.width) / 2 +
                                6,
                            node.y,
                        )
                    )
                labelItems.addAll(edgeLabel(loop.label, at))
            }
        }
    }

    for (node in nodes.values) {
        val source = node.node ?: continue
        val box = node.box ?: continue
        val center = transform(Pt(node.x, node.y))
        val kind = family(source.shape)
        val style = source.style
        val solid = source.shape == NodeShape.START || source.shape == NodeShape.FORK
        val paths = shapePaths(source.shape, center.x, center.y, box.w, box.h)
        val fill: DiagramPaint =
            style?.fill?.let { DiagramPaint.Literal(it) } ?: if (solid) tone("ink") else tone(kind)
        val stroke: DiagramPaint =
            style?.stroke?.let { DiagramPaint.Literal(it) }
                ?: if (solid) tone("ink") else tone("$kind-line")
        for (d in paths.outlines) {
            nodeItems.add(
                PathItem(
                    d = d,
                    fill = fill,
                    stroke = stroke,
                    width =
                        style?.strokeWidth ?: (if (source.shape == NodeShape.END) 1.5 else 1.25),
                    dash = if (style?.dashed == true) listOf(5.0, 4.0) else null,
                    link = source.link,
                    shadow = !(solid || source.shape == NodeShape.END),
                )
            )
        }

        for (d in paths.details) {
            val filled = source.shape == NodeShape.END
            nodeItems.add(
                PathItem(
                    d = d,
                    fill = if (filled) tone("ink") else null,
                    stroke = if (filled) tone("ink") else stroke,
                    width = 1.25,
                    link = source.link,
                )
            )
        }

        if (
            box.text.lines.isNotEmpty() &&
                source.shape != NodeShape.START &&
                source.shape != NodeShape.END &&
                source.shape != NodeShape.FORK &&
                source.shape != NodeShape.CHOICE
        ) {
            nodeItems.add(
                TextItem(
                    x = center.x,
                    y = center.y + (if (source.shape == NodeShape.CYLINDER) 5 else 0),
                    lines = box.text.lines,
                    size = NODE_FONT,
                    weight = 500,
                    anchor = TextAnchor.MIDDLE,
                    fill = textOn(style, "$kind-ink"),
                    link = source.link,
                )
            )
        }
    }

    items.addAll(backgrounds)
    items.addAll(edgeItems)
    items.addAll(titleItems)
    items.addAll(labelItems)
    items.addAll(nodeItems)
    val count = graph.nodes.size
    return fitScene(
        Scene(
            width,
            height,
            items,
            graph.title ?: "Flowchart with $count ${if (count == 1) "node" else "nodes"}",
        ),
        measure,
    )
}

/**
 * A smooth curve through layout points. Ends leave along the rank axis; interior points take
 * Catmull-Rom tangents, so collinear runs stay straight.
 */
private fun smoothPath(points: List<Pt>, transform: (Pt) -> Pt): List<PathCommand> {
    val tangents = points.mapIndexed { index, point ->
        when (index) {
            0 -> Pt(0.0, (points[1].y - point.y) * 1.5)
            points.size - 1 -> Pt(0.0, (point.y - points[index - 1].y) * 1.5)
            else -> {
                val previous = points[index - 1]
                val next = points[index + 1]
                Pt((next.x - previous.x) / 2, (next.y - previous.y) / 2)
            }
        }
    }
    val start = transform(points[0])
    val d = mutableListOf<PathCommand>(PathCommand.Move(start.x, start.y))
    for (index in 1 until points.size) {
        val a = points[index - 1]
        val b = points[index]
        val m = tangents[index - 1]
        val n = tangents[index]
        val control1 = transform(Pt(a.x + m.x / 3, a.y + m.y / 3))
        val control2 = transform(Pt(b.x - n.x / 3, b.y - n.y / 3))
        val end = transform(b)
        d.add(PathCommand.Cubic(control1.x, control1.y, control2.x, control2.y, end.x, end.y))
    }

    return d
}

private fun edgeLabel(label: TextBlock, at: Pt): List<SceneItem> {
    val w = label.width + 12
    val h = label.height + 4
    return listOf(
        PathItem(d = roundedRect(at.x - w / 2, at.y - h / 2, w, h, 6.0), fill = tone("label")),
        TextItem(
            x = at.x,
            y = at.y,
            lines = label.lines,
            size = LABEL_FONT,
            weight = 500,
            anchor = TextAnchor.MIDDLE,
            fill = tone("muted"),
        ),
    )
}

// ---------------------------------------------------------------------------
// Sequence diagrams.

private const val SEQ_FONT = 13.0
private const val ACTOR_GAP = 44.0

private class Head(val participant: Participant, val text: TextBlock, val w: Double, val h: Double)

private class Prepared(val item: SequenceItem, val text: TextBlock?)

private class Activation(val column: Int, val start: Double, val end: Double, val depth: Int)

private class FrameBranch(val y: Double, val label: TextBlock?)

private class Frame(
    val kind: BlockKind,
    val label: TextBlock?,
    val start: Double,
    val color: String?,
    val depth: Int,
) {
    val branches = mutableListOf<FrameBranch>()
    val columns = LinkedHashSet<Int>()
    var low = Double.POSITIVE_INFINITY
    var high = Double.NEGATIVE_INFINITY
    var end = 0.0
}

internal fun layoutSequence(diagram: SequenceDiagram, measure: TextMeasure): Scene {
    if (diagram.participants.isEmpty()) throw DiagramError("This diagram has nothing to draw.")
    val items = mutableListOf<SceneItem>()
    val titleBlock = diagram.title?.let { textBlock(it, TITLE_FONT, 600, 480.0, measure) }
    val top = MARGIN + (if (titleBlock != null) titleBlock.height + 14 else 0.0)
    val index = HashMap<String, Int>()
    diagram.participants.forEachIndexed { position, participant ->
        index[participant.id] = position
    }
    val heads =
        diagram.participants.map { participant ->
            val text = textBlock(participant.label, NODE_FONT, 600, 150.0, measure)
            Head(
                participant,
                text,
                max(text.width + 32, 96.0),
                text.height + (if (participant.actor) 48 else 22),
            )
        }
    val headHeight = heads.fold(0.0) { tallest, head -> max(tallest, head.h) }

    // Column gaps grow until every message and note fits between its participants.
    val gaps =
        heads
            .drop(1)
            .mapIndexed { position, head -> (heads[position].w + head.w) / 2 + ACTOR_GAP }
            .toMutableList()
    var leftRoom = 0.0
    var rightRoom = 0.0
    fun need(from: Int, to: Int, distance: Double) {
        if (from == to) return
        val a = min(from, to)
        val b = max(from, to)
        var have = 0.0
        for (column in a until b) have += gaps[column]
        if (have < distance) gaps[b - 1] += distance - have
    }

    val prepared =
        diagram.items.map { item ->
            when (item) {
                is SequenceMessage -> {
                    val text = textBlock(item.label, SEQ_FONT, 400, 260.0, measure)
                    val from = index.getValue(item.from)
                    val to = index.getValue(item.to)
                    val numbered = if (item.number == null) 0.0 else 22.0
                    if (from == to) {
                        if (from + 1 < heads.size) need(from, from + 1, text.width + 64 + numbered)
                        else rightRoom = max(rightRoom, text.width + 64 + numbered)
                    } else {
                        need(from, to, text.width + 36 + numbered)
                    }

                    Prepared(item, text)
                }
                is SequenceNote -> {
                    val text = textBlock(item.label, SEQ_FONT, 400, 220.0, measure)
                    val ids = item.participants.map { index.getValue(it) }
                    val w = text.width + 24
                    when {
                        item.placement == NotePlacement.LEFT ->
                            if (ids[0] > 0)
                                need(ids[0] - 1, ids[0], w + 24 + heads[ids[0] - 1].w / 2)
                            else leftRoom = max(leftRoom, w + 12 - heads[0].w / 2)
                        item.placement == NotePlacement.RIGHT ->
                            if (ids[0] + 1 < heads.size)
                                need(ids[0], ids[0] + 1, w + 24 + heads[ids[0] + 1].w / 2)
                            else rightRoom = max(rightRoom, w + 12 - heads.last().w / 2)
                        else -> {
                            val first = ids.min()
                            val last = ids.max()
                            if (first != last) {
                                need(first, last, w - 40)
                            } else {
                                val sideRoom = max(0.0, (w - heads[first].w) / 2)
                                if (first == 0) leftRoom = max(leftRoom, sideRoom)
                                else need(first - 1, first, w / 2 + heads[first - 1].w / 2 + 12)
                                if (last == heads.size - 1) rightRoom = max(rightRoom, sideRoom)
                                else need(last, last + 1, w / 2 + heads[last + 1].w / 2 + 12)
                            }
                        }
                    }

                    Prepared(item, text)
                }
                is SequenceBlock -> {
                    val label = if (item.label.isNotEmpty()) "[${item.label}]" else ""
                    Prepared(item, textBlock(label, LABEL_FONT, 500, 280.0, measure))
                }
                is SequenceBranch -> {
                    val label = if (item.label.isNotEmpty()) "[${item.label}]" else ""
                    Prepared(item, textBlock(label, LABEL_FONT, 500, 280.0, measure))
                }
                else -> Prepared(item, null)
            }
        }

    val centers = mutableListOf<Double>()
    var x = MARGIN + max(0.0, leftRoom) + (heads.firstOrNull()?.w ?: 0.0) / 2
    for (column in heads.indices) {
        centers.add(x)
        x += gaps.getOrNull(column) ?: 0.0
    }

    val right =
        (centers.lastOrNull() ?: 0.0) +
            (heads.lastOrNull()?.w ?: 0.0) / 2 +
            max(0.0, rightRoom) +
            MARGIN

    // Rows.
    val rows = mutableListOf<SceneItem>()
    val frames = mutableListOf<SceneItem>()
    val bars = mutableListOf<SceneItem>()
    val active = diagram.participants.map { mutableListOf<Double>() }
    val activations = mutableListOf<Activation>()
    val stack = mutableListOf<Frame>()
    fun frameStartX(frame: Frame): Double {
        val columns =
            if (frame.columns.isNotEmpty()) frame.columns.toList() else centers.indices.toList()
        val columnLeft = minimum(columns.map { centers[it] }) - 52 + frame.depth * 6
        return max(MARGIN / 2 + frame.depth * 6, min(columnLeft, frame.low - 14))
    }
    val finished = mutableListOf<Frame>()
    var y = top + headHeight + 26

    fun touch(vararg columns: Int) {
        for (frame in stack) {
            for (column in columns) frame.columns.add(column)
        }
    }

    // Frames also widen to the text drawn inside them.
    fun extend(low: Double, high: Double) {
        for (frame in stack) {
            frame.low = min(frame.low, low)
            frame.high = max(frame.high, high)
        }
    }

    fun activate(column: Int, at: Double) {
        active[column].add(at)
    }

    fun deactivate(column: Int, at: Double) {
        val start = active[column].removeLastOrNull()
        if (start != null) activations.add(Activation(column, start, at, active[column].size))
    }

    fun edgeX(column: Int, toward: Double): Double {
        val depth = active[column].size
        if (depth == 0) return centers[column]
        return centers[column] + (if (toward >= centers[column]) 5 else -5) + (depth - 1) * 4
    }

    for ((item, text) in prepared.map { it.item to it.text }) {
        when (item) {
            is SequenceMessage -> {
                val from = index.getValue(item.from)
                val to = index.getValue(item.to)
                touch(from, to)
                val block = text!!
                val paint = tone("edge")
                val dash = if (item.line == EdgeLine.DOTTED) listOf(5.0, 4.0) else null
                val numbered = if (item.number == null) 0.0 else 22.0
                if (from == to) {
                    val labelY = y + block.height / 2
                    y += max(block.height, 4.0) + 6
                    val x0 = edgeX(from, Double.POSITIVE_INFINITY)
                    val loopTop = y
                    val loopBottom = y + 22
                    if (item.activate) activate(to, loopBottom - 4)
                    val x1 = edgeX(to, Double.POSITIVE_INFINITY)
                    val inset =
                        if (item.head == MessageHead.ARROW || item.head == MessageHead.ASYNC) ARROW
                        else 0.0
                    rows.add(
                        PathItem(
                            d =
                                listOf(
                                    PathCommand.Move(x0, loopTop),
                                    PathCommand.Line(x0 + 30, loopTop),
                                    PathCommand.Cubic(
                                        x0 + 40,
                                        loopTop,
                                        x0 + 40,
                                        loopBottom,
                                        x0 + 30,
                                        loopBottom,
                                    ),
                                    PathCommand.Line(x1 + inset, loopBottom),
                                ),
                            stroke = paint,
                            width = 1.5,
                            dash = dash,
                        )
                    )
                    rows.addAll(messageHead(item.head, Pt(x1, loopBottom), Pt(-1.0, 0.0), paint))
                    if (item.both)
                        rows.addAll(messageHead(item.head, Pt(x0, loopTop), Pt(-1.0, 0.0), paint))
                    rows.addAll(messageText(block, x0 + 12 + numbered, labelY, TextAnchor.START))
                    extend(x0, max(x0 + 44, x0 + 12 + numbered + block.width))
                    if (item.number != null)
                        rows.addAll(badge(item.number, x0 + 12, labelY, measure))
                    y = loopBottom + 16
                } else {
                    val labelY = y + block.height / 2
                    y += max(block.height, 4.0) + 6
                    if (item.activate) activate(to, y)
                    val direction = if (to > from) 1.0 else -1.0
                    val x0 = edgeX(from, centers[to])
                    val x1 = edgeX(to, centers[from])
                    val inset =
                        when (item.head) {
                            MessageHead.ARROW,
                            MessageHead.ASYNC -> ARROW
                            MessageHead.CROSS -> 2.0
                            else -> 0.0
                        }
                    val startInset = if (item.both) inset else 0.0
                    rows.add(
                        PathItem(
                            d =
                                listOf(
                                    PathCommand.Move(x0 + direction * startInset, y),
                                    PathCommand.Line(x1 - direction * inset, y),
                                ),
                            stroke = paint,
                            width = 1.5,
                            dash = dash,
                        )
                    )
                    rows.addAll(messageHead(item.head, Pt(x1, y), Pt(direction, 0.0), paint))
                    if (item.both)
                        rows.addAll(messageHead(item.head, Pt(x0, y), Pt(-direction, 0.0), paint))
                    val middle = (x0 + x1) / 2
                    rows.addAll(
                        messageText(
                            block,
                            middle + (if (item.number == null) 0.0 else 11.0),
                            labelY,
                            TextAnchor.MIDDLE,
                        )
                    )
                    extend(
                        min(min(x0, x1), middle - block.width / 2),
                        max(max(x0, x1), middle + block.width / 2 + 11),
                    )
                    if (item.number != null)
                        rows.addAll(badge(item.number, x0 + direction * 14, y, measure))
                    y += 18
                }

                if (item.deactivate) deactivate(from, y - 18)
            }
            is SequenceActivation ->
                if (item.active) activate(index.getValue(item.participant), y - 12)
                else deactivate(index.getValue(item.participant), y - 12)
            is SequenceNote -> {
                val block = text!!
                val ids = item.participants.map { index.getValue(it) }
                touch(*ids.toIntArray())
                val w = block.width + 24
                val h = block.height + 14
                val left: Double
                var width = w
                when (item.placement) {
                    NotePlacement.LEFT -> left = centers[ids[0]] - 14 - w
                    NotePlacement.RIGHT -> left = centers[ids[0]] + 14
                    NotePlacement.OVER -> {
                        val a = minimum(ids.map { centers[it] })
                        val b = maximum(ids.map { centers[it] })
                        width = max(w, b - a + 40)
                        left = (a + b) / 2 - width / 2
                    }
                }

                rows.add(
                    PathItem(
                        d =
                            polygon(
                                Pt(left, y),
                                Pt(left + width - 10, y),
                                Pt(left + width, y + 10),
                                Pt(left + width, y + h),
                                Pt(left, y + h),
                            ),
                        fill = tone("note"),
                        stroke = tone("note-line"),
                        width = 1.0,
                    )
                )
                rows.add(
                    PathItem(
                        d =
                            listOf(
                                PathCommand.Move(left + width - 10, y),
                                PathCommand.Line(left + width - 10, y + 10),
                                PathCommand.Line(left + width, y + 10),
                            ),
                        stroke = tone("note-line"),
                        width = 1.0,
                    )
                )
                rows.add(
                    TextItem(
                        x = left + width / 2,
                        y = y + h / 2,
                        lines = block.lines,
                        size = SEQ_FONT,
                        weight = 400,
                        anchor = TextAnchor.MIDDLE,
                        fill = tone("note-ink"),
                    )
                )
                extend(left, left + width)
                y += h + 14
            }
            is SequenceBlock -> {
                y += 4
                stack.add(
                    Frame(
                        kind = item.kind,
                        label = text!!.takeIf { it.lines.isNotEmpty() },
                        start = y,
                        color = item.color,
                        depth = stack.size,
                    )
                )
                y += if (item.kind == BlockKind.RECT) 12.0 else max(24.0, text.height + 10)
            }
            is SequenceBranch -> {
                val frame = stack.lastOrNull()
                if (frame != null) {
                    y += 4
                    frame.branches.add(FrameBranch(y, text!!.takeIf { it.lines.isNotEmpty() }))
                    y += max(22.0, text.height + 8)
                }
            }
            SequenceEnd -> {
                val frame = stack.removeLastOrNull()
                if (frame != null) {
                    y += 6
                    frame.end = y
                    if (frame.kind != BlockKind.RECT) {
                        val left = frameStartX(frame)
                        val tagWidth = measure.width(frame.kind.id, LABEL_FONT, 700) + 18
                        val headerRight = left + tagWidth + 10 + (frame.label?.width ?: 0.0)
                        val branchRight =
                            left +
                                10 +
                                (frame.branches.maxOfOrNull { it.label?.width ?: 0.0 } ?: 0.0)
                        frame.high = max(frame.high, max(headerRight, branchRight))
                    }
                    finished.add(frame)
                    for (parent in stack) {
                        parent.columns.addAll(frame.columns)
                        parent.low = min(parent.low, frame.low - 8)
                        parent.high = max(parent.high, frame.high + 8)
                    }

                    y += 10
                }
            }
        }
    }

    for (column in active.indices) {
        while (active[column].isNotEmpty()) deactivate(column, y - 6)
    }

    val bottomTop = y + 8

    // Frames are drawn deepest last, inset by their depth so nested frames stay readable.
    var widest = right
    finished.sortWith { a, b -> a.depth - b.depth }
    for (frame in finished) {
        val columns =
            if (frame.columns.isNotEmpty()) frame.columns.toList() else centers.indices.toList()
        val frameLeft = frameStartX(frame)
        var frameRight = maximum(columns.map { centers[it] }) + 52 - frame.depth * 6
        frameRight = max(max(frameRight, frameLeft + 140), frame.high + 14)
        widest = max(widest, frameRight + MARGIN / 2)
        val left = frameLeft
        val frameEnd = frameRight
        if (frame.kind == BlockKind.RECT) {
            frames.add(
                0,
                PathItem(
                    d =
                        roundedRect(
                            left,
                            frame.start,
                            frameEnd - left,
                            frame.end - frame.start,
                            8.0,
                        ),
                    fill = frame.color?.let { DiagramPaint.Literal(it) } ?: tone("cluster"),
                    opacity = if (frame.color != null) 0.22 else 1.0,
                ),
            )
            continue
        }

        frames.add(
            PathItem(
                d = roundedRect(left, frame.start, frameEnd - left, frame.end - frame.start, 8.0),
                stroke = tone("cluster-line"),
                width = 1.25,
            )
        )
        val tag = frame.kind.id
        val tagWidth = measure.width(tag, LABEL_FONT, 700) + 18
        frames.add(
            PathItem(
                d =
                    listOf(
                        PathCommand.Move(left + 8, frame.start),
                        PathCommand.Line(left + tagWidth, frame.start),
                        PathCommand.Line(left + tagWidth, frame.start + 14),
                        PathCommand.Line(left + tagWidth - 6, frame.start + 20),
                        PathCommand.Line(left, frame.start + 20),
                        PathCommand.Line(left, frame.start + 8),
                        PathCommand.Cubic(
                            left,
                            frame.start + 3.6,
                            left + 3.6,
                            frame.start,
                            left + 8,
                            frame.start,
                        ),
                        PathCommand.Close,
                    ),
                fill = tone("cluster-line"),
            )
        )
        frames.add(
            TextItem(
                x = left + 8,
                y = frame.start + 10,
                lines = listOf(tag),
                size = LABEL_FONT,
                weight = 700,
                anchor = TextAnchor.START,
                fill = tone("cluster-ink"),
            )
        )
        if (frame.label != null) {
            frames.add(
                TextItem(
                    x = left + tagWidth + 10,
                    y = frame.start + 10 + (frame.label.height - lineHeight(LABEL_FONT)) / 2,
                    lines = frame.label.lines,
                    size = LABEL_FONT,
                    weight = 500,
                    anchor = TextAnchor.START,
                    fill = tone("muted"),
                )
            )
        }

        for (branch in frame.branches) {
            frames.add(
                PathItem(
                    d =
                        listOf(
                            PathCommand.Move(left, branch.y),
                            PathCommand.Line(frameEnd, branch.y),
                        ),
                    stroke = tone("cluster-line"),
                    width = 1.0,
                    dash = listOf(5.0, 4.0),
                )
            )
            if (branch.label != null) {
                frames.add(
                    TextItem(
                        x = left + 10,
                        y = branch.y + 4 + branch.label.height / 2,
                        lines = branch.label.lines,
                        size = LABEL_FONT,
                        weight = 500,
                        anchor = TextAnchor.START,
                        fill = tone("muted"),
                    )
                )
            }
        }
    }

    for (activation in activations) {
        val center = centers[activation.column] + activation.depth * 4
        bars.add(
            PathItem(
                d =
                    roundedRect(
                        center - 5,
                        activation.start,
                        10.0,
                        max(12.0, activation.end - activation.start),
                        3.0,
                    ),
                fill = tone("data"),
                stroke = tone("data-line"),
                width = 1.0,
            )
        )
    }

    val lifelines = centers.map { center ->
        PathItem(
            d =
                listOf(
                    PathCommand.Move(center, top + headHeight),
                    PathCommand.Line(center, bottomTop),
                ),
            stroke = tone("cluster-line"),
            width = 1.25,
            dash = listOf(3.0, 4.0),
        )
    }

    val participants = mutableListOf<SceneItem>()
    for ((column, head) in heads.withIndex()) {
        for (edge in listOf(top, bottomTop)) {
            val cx = centers[column]
            if (head.participant.actor) {
                val figureTop = edge + (headHeight - head.h) / 2
                participants.add(
                    PathItem(
                        d = ellipse(cx, figureTop + 9, 7.0, 7.0),
                        fill = tone("terminal"),
                        stroke = tone("terminal-line"),
                        width = 1.5,
                    )
                )
                participants.add(
                    PathItem(
                        d =
                            listOf(
                                PathCommand.Move(cx - 13, figureTop + 34),
                                PathCommand.Cubic(
                                    cx - 13,
                                    figureTop + 22,
                                    cx + 13,
                                    figureTop + 22,
                                    cx + 13,
                                    figureTop + 34,
                                ),
                                PathCommand.Close,
                            ),
                        fill = tone("terminal"),
                        stroke = tone("terminal-line"),
                        width = 1.5,
                    )
                )
                participants.add(
                    TextItem(
                        x = cx,
                        y = figureTop + 40 + head.text.height / 2,
                        lines = head.text.lines,
                        size = NODE_FONT,
                        weight = 600,
                        anchor = TextAnchor.MIDDLE,
                        fill = tone("ink"),
                    )
                )
                continue
            }

            participants.add(
                PathItem(
                    d = roundedRect(cx - head.w / 2, edge, head.w, headHeight, 10.0),
                    fill = tone("terminal"),
                    stroke = tone("terminal-line"),
                    width = 1.25,
                    shadow = true,
                )
            )
            participants.add(
                TextItem(
                    x = cx,
                    y = edge + headHeight / 2,
                    lines = head.text.lines,
                    size = NODE_FONT,
                    weight = 600,
                    anchor = TextAnchor.MIDDLE,
                    fill = tone("terminal-ink"),
                )
            )
        }
    }

    val width = ceil(max(widest, if (titleBlock != null) titleBlock.width + MARGIN * 2 else 0.0))
    if (titleBlock != null) {
        items.add(
            TextItem(
                x = width / 2,
                y = MARGIN + titleBlock.height / 2,
                lines = titleBlock.lines,
                size = TITLE_FONT,
                weight = 600,
                anchor = TextAnchor.MIDDLE,
                fill = tone("ink"),
            )
        )
    }

    fun translucent(item: SceneItem) = item is PathItem && item.opacity != null
    items.addAll(frames.filter(::translucent))
    items.addAll(lifelines)
    items.addAll(bars)
    items.addAll(frames.filterNot(::translucent))
    items.addAll(rows)
    items.addAll(participants)
    val count = diagram.participants.size
    return Scene(
        width,
        ceil(bottomTop + headHeight + MARGIN),
        items,
        diagram.title
            ?: "Sequence diagram with $count ${if (count == 1) "participant" else "participants"}",
    )
}

private fun messageText(
    block: TextBlock,
    x: Double,
    y: Double,
    anchor: TextAnchor,
): List<SceneItem> {
    if (block.lines.isEmpty()) return emptyList()
    return listOf(
        TextItem(
            x = x,
            y = y,
            lines = block.lines,
            size = SEQ_FONT,
            weight = 400,
            anchor = anchor,
            fill = tone("ink"),
        )
    )
}

private fun badge(value: Double, x: Double, y: Double, measure: TextMeasure): List<SceneItem> {
    val text = jsNumberString(value)
    val w = max(18.0, measure.width(text, 10.0, 700) + 10)
    return listOf(
        PathItem(d = roundedRect(x - w / 2, y - 9, w, 18.0, 9.0), fill = tone("edge")),
        TextItem(
            x = x,
            y = y,
            lines = listOf(text),
            size = 10.0,
            weight = 700,
            anchor = TextAnchor.MIDDLE,
            fill = tone("canvas"),
        ),
    )
}

private fun messageHead(
    head: MessageHead,
    tip: Pt,
    direction: Pt,
    paint: DiagramPaint,
): List<SceneItem> {
    val ux = direction.x
    return when (head) {
        MessageHead.ARROW -> headPaths(EdgeHead.ARROW, tip, direction, paint, 1.5)
        MessageHead.CROSS ->
            headPaths(EdgeHead.CROSS, Pt(tip.x + ux * 3, tip.y), direction, paint, 1.5)
        MessageHead.ASYNC ->
            listOf(
                PathItem(
                    d =
                        listOf(
                            PathCommand.Move(tip.x - ux * 9, tip.y - 5),
                            PathCommand.Line(tip.x, tip.y),
                            PathCommand.Line(tip.x - ux * 9, tip.y + 5),
                        ),
                    stroke = paint,
                    width = 1.5,
                )
            )
        else -> emptyList()
    }
}

// ---------------------------------------------------------------------------
// Pie charts: a donut with a legend beside it.

private const val SERIES = 8

internal fun layoutPie(diagram: PieDiagram, measure: TextMeasure): Scene {
    val items = mutableListOf<SceneItem>()
    val titleBlock = diagram.title?.let { textBlock(it, TITLE_FONT, 600, 480.0, measure) }
    val top = MARGIN + (if (titleBlock != null) titleBlock.height + 16 else 0.0)
    val total = diagram.slices.fold(0.0) { sum, slice -> sum + slice.value }
    val radius = 92.0
    val inner = 56.0
    val cx = MARGIN + radius
    val visible = diagram.slices.filter { it.value > 0 }
    val rowHeight = 24.0
    val legendHeight = visible.size * rowHeight
    val cy = top + max(radius, legendHeight / 2)
    fun format(value: Double) = jsLocaleNumber(jsRound(value * 100) / 100)

    var angle = 0.0
    visible.forEachIndexed { position, slice ->
        val sweep = (slice.value / total) * PI * 2
        val paint = tone("series-${position % SERIES}")
        // Slices are separated by a hairline gap of equal width on both rims.
        val gap = if (visible.size == 1) 0.0 else min(0.012, sweep / 4)
        items.add(
            PathItem(
                d =
                    arc(cx, cy, radius, angle + gap, angle + sweep - gap, true) +
                        arc(
                            cx,
                            cy,
                            inner,
                            angle + sweep - gap * radius / inner,
                            angle + gap * radius / inner,
                            false,
                        ) +
                        PathCommand.Close,
                fill = paint,
            )
        )

        val share = slice.value / total
        if (share >= 0.06) {
            val middle = angle + sweep / 2
            val ring = (radius + inner) / 2
            items.add(
                TextItem(
                    x = cx + ring * sin(middle),
                    y = cy - ring * cos(middle),
                    lines = listOf("${jsNumberString(jsRound(share * 100))}%"),
                    size = 11.0,
                    weight = 700,
                    anchor = TextAnchor.MIDDLE,
                    fill = tone("canvas"),
                )
            )
        }

        angle += sweep
    }

    items.add(
        TextItem(
            x = cx,
            y = cy - 8,
            lines = listOf(format(total)),
            size = 18.0,
            weight = 700,
            anchor = TextAnchor.MIDDLE,
            fill = tone("ink"),
        )
    )
    items.add(
        TextItem(
            x = cx,
            y = cy + 12,
            lines = listOf("total"),
            size = 11.0,
            weight = 500,
            anchor = TextAnchor.MIDDLE,
            fill = tone("muted"),
        )
    )

    val legendX = cx + radius + 32
    var legendY = cy - legendHeight / 2 + rowHeight / 2
    var legendWidth = 0.0
    for ((position, slice) in visible.withIndex()) {
        val share = jsRound((slice.value / total) * 1000) / 10
        val label = slice.label
        val detail =
            if (diagram.showData) "${jsNumberString(share)}% · ${format(slice.value)}"
            else "${jsNumberString(share)}%"
        items.add(
            PathItem(
                d = roundedRect(legendX, legendY - 6, 12.0, 12.0, 3.5),
                fill = tone("series-${position % SERIES}"),
            )
        )
        items.add(
            TextItem(
                x = legendX + 20,
                y = legendY,
                lines = listOf(label),
                size = 13.0,
                weight = 500,
                anchor = TextAnchor.START,
                fill = tone("ink"),
            )
        )
        val labelWidth = measure.width(label, 13.0, 500)
        items.add(
            TextItem(
                x = legendX + 20 + labelWidth + 8,
                y = legendY,
                lines = listOf(detail),
                size = 12.0,
                weight = 500,
                anchor = TextAnchor.START,
                fill = tone("muted"),
            )
        )
        legendWidth = max(legendWidth, 20 + labelWidth + 8 + measure.width(detail, 12.0, 500))
        legendY += rowHeight
    }

    val width =
        ceil(
            max(
                legendX + legendWidth + MARGIN,
                if (titleBlock != null) titleBlock.width + MARGIN * 2 else 0.0,
            )
        )
    if (titleBlock != null) {
        items.add(
            0,
            TextItem(
                x = width / 2,
                y = MARGIN + titleBlock.height / 2,
                lines = titleBlock.lines,
                size = TITLE_FONT,
                weight = 600,
                anchor = TextAnchor.MIDDLE,
                fill = tone("ink"),
            ),
        )
    }

    return Scene(
        width,
        ceil(max(cy + radius, cy + legendHeight / 2) + MARGIN),
        items,
        diagram.title
            ?: "Pie chart with ${visible.size} ${if (visible.size == 1) "slice" else "slices"}",
    )
}
