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
            val offsets = mutableListOf(0)
            var offset = 0
            while (offset < word.length) {
                offset += Character.charCount(word.codePointAt(offset))
                offsets.add(offset)
            }
            val characters = offsets.size - 1
            var start = 0
            while (start < characters) {
                val remaining = characters - start
                var low = 1
                var high = min(2, remaining)
                // Grow from the start of each segment, never rescan the entire remaining word.
                while (
                    high < remaining &&
                        measure.width(
                            word.substring(offsets[start], offsets[start + high]),
                            size,
                            weight,
                        ) <= max
                ) {
                    low = high
                    high = min(high * 2, remaining)
                }
                if (
                    measure.width(
                        word.substring(offsets[start], offsets[start + high]),
                        size,
                        weight,
                    ) <= max
                ) {
                    low = high
                } else {
                    while (high - low > 1) {
                        val middle = (low + high) / 2
                        if (
                            measure.width(
                                word.substring(offsets[start], offsets[start + middle]),
                                size,
                                weight,
                            ) <= max
                        )
                            low = middle
                        else high = middle
                    }
                }
                val segment = word.substring(offsets[start], offsets[start + low])
                start += low
                if (start < characters) lines.add(segment) else line = segment
            }
        }

        lines.add(line)
    }

    while (lines.size > 1 && lines.last().isEmpty()) lines.removeAt(lines.size - 1)
    return lines
}

internal class TextBlock(val lines: List<String>, val width: Double, val height: Double)

internal fun textBlock(
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
internal fun minimum(values: Iterable<Double>) = values.fold(Double.POSITIVE_INFINITY, ::min)

/** `Math.max(...values)`: negative infinite when empty. */
internal fun maximum(values: Iterable<Double>) = values.fold(Double.NEGATIVE_INFINITY, ::max)

/** A JavaScript sort comparator's result. */
internal fun sortOrder(difference: Double) =
    if (difference < 0) -1 else if (difference > 0) 1 else 0

// Path helpers. Quarter ellipse arcs use the usual cubic approximation.

internal fun roundedRect(
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

internal fun ellipse(cx: Double, cy: Double, rx: Double, ry: Double): List<PathCommand> {
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

internal class Pt(val x: Double, val y: Double)

internal fun polygon(vararg points: Pt): List<PathCommand> =
    listOf(PathCommand.Move(points[0].x, points[0].y)) +
        points.drop(1).map { PathCommand.Line(it.x, it.y) } +
        PathCommand.Close

/** Arc from angle a0 to a1 (radians, clockwise from 12 o'clock), as cubic segments. */
internal fun arc(
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
internal fun luminance(color: String): Double? {
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

internal fun textOn(style: DiagramStyle?, fallback: String): DiagramPaint {
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

    return diagramTone(fallback)
}

internal fun family(shape: NodeShape) =
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

internal class NodeBox(val w: Double, val h: Double, val text: TextBlock)

internal fun nodeBox(node: GraphNode, horizontal: Boolean, measure: TextMeasure): NodeBox {
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
        // Keep the centered label beyond the inward notch, whose depth is h / 3.
        NodeShape.FLAG -> box(tw + 44 + (th + 22) * 2 / 3, th + 22)
        NodeShape.STADIUM -> box(tw + 28 + th, th + 22)
        NodeShape.NOTE -> box(tw + 36, th + 24)
        else -> box(max(tw + 32, 56.0), th + 22)
    }
}

internal class ShapePaths(
    val outlines: List<List<PathCommand>>,
    val details: List<List<PathCommand>>,
)

internal fun shapePaths(
    shape: NodeShape,
    cx: Double,
    cy: Double,
    w: Double,
    h: Double,
): ShapePaths {
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

/** Intersect canonical node contours with a ray in the graph's layout coordinates. */
internal fun outlineExtents(
    outlines: List<List<PathCommand>>,
    direction: Direction,
    axis: Char,
    side: Int,
    offsets: List<Double>,
): List<Double> {
    fun project(x: Double, y: Double): Pt {
        val point =
            when (direction) {
                Direction.BT -> Pt(x, -y)
                Direction.LR -> Pt(y, x)
                Direction.RL -> Pt(y, -x)
                else -> Pt(x, y)
            }
        return if (axis == 'x') Pt(side * point.x, point.y) else Pt(side * point.y, point.x)
    }
    fun cubic(a: Double, b: Double, c: Double, d: Double, t: Double): Double {
        val u = 1 - t
        return u * u * u * a + 3 * u * u * t * b + 3 * u * t * t * c + t * t * t * d
    }
    return offsets.map { offset ->
        var extent = Double.NEGATIVE_INFINITY
        for (path in outlines) {
            var current = Pt(0.0, 0.0)
            var first = current
            fun advance(end: Pt, c1: Pt? = null, c2: Pt? = null) {
                if (offset >= min(current.y, end.y) && offset <= max(current.y, end.y)) {
                    if (current.y == end.y) extent = max(extent, max(current.x, end.x))
                    else if (c1 != null && c2 != null) {
                        // Generated outline cubics are monotone on both axes; bisection stays
                        // bounded.
                        var low = 0.0
                        var high = 1.0
                        repeat(32) {
                            val middle = (low + high) / 2
                            val value = cubic(current.y, c1.y, c2.y, end.y, middle)
                            if ((value < offset) == (end.y > current.y)) low = middle
                            else high = middle
                        }
                        extent = max(extent, cubic(current.x, c1.x, c2.x, end.x, (low + high) / 2))
                    } else {
                        val t = (offset - current.y) / (end.y - current.y)
                        extent = max(extent, current.x + (end.x - current.x) * t)
                    }
                }
                current = end
            }
            for (command in path) when (command) {
                is PathCommand.Move -> {
                    current = project(command.x, command.y)
                    first = current
                }
                is PathCommand.Line -> advance(project(command.x, command.y))
                is PathCommand.Cubic ->
                    advance(
                        project(command.x, command.y),
                        project(command.x1, command.y1),
                        project(command.x2, command.y2),
                    )
                PathCommand.Close -> advance(first)
            }
        }
        if (!extent.isFinite()) throw DiagramError("This node outline cannot be drawn.")
        extent
    }
}

internal fun headPaths(
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
                fill = diagramTone("canvas"),
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
internal fun headInset(head: EdgeHead) =
    when (head) {
        EdgeHead.ARROW -> ARROW
        EdgeHead.CIRCLE -> 8.5
        EdgeHead.CROSS -> 1.0
        EdgeHead.NONE -> 0.0
    }

/** Include every drawn primitive, including loop labels that extend beyond ranked nodes. */
internal fun fitScene(scene: Scene, measure: TextMeasure): Scene {
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
