// Mermaid diagrams in Markdown fences, parsed into a small model that both the Android and web
// renderers lay out natively. Keep this file in step with the web's apps/web/src/diagram.ts:
// tests/fixtures/diagrams.json pins the parse results of both implementations.

package build.cairn.app.ui

import java.math.BigDecimal
import java.math.BigInteger
import java.math.RoundingMode
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.min
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

internal enum class Direction {
    TB,
    BT,
    LR,
    RL,
}

internal enum class NodeShape(val id: String) {
    RECT("rect"),
    ROUND("round"),
    STADIUM("stadium"),
    SUBROUTINE("subroutine"),
    CYLINDER("cylinder"),
    CIRCLE("circle"),
    DOUBLE_CIRCLE("doublecircle"),
    DIAMOND("diamond"),
    HEXAGON("hexagon"),
    PARALLELOGRAM("parallelogram"),
    PARALLELOGRAM_ALT("parallelogram-alt"),
    TRAPEZOID("trapezoid"),
    TRAPEZOID_ALT("trapezoid-alt"),
    FLAG("flag"),
    NOTE("note"),
    START("start"),
    END("end"),
    FORK("fork"),
    CHOICE("choice"),
}

/** Author colors are kept only when they are plain CSS colors. */
internal data class DiagramStyle(
    val fill: String? = null,
    val stroke: String? = null,
    val color: String? = null,
    val strokeWidth: Double? = null,
    val dashed: Boolean? = null,
) {
    val isEmpty
        get() =
            fill == null && stroke == null && color == null && strokeWidth == null && dashed == null
}

internal class GraphNode(val id: String, var label: String, var shape: NodeShape) {
    var parent: String? = null
    var style: DiagramStyle? = null
    var link: String? = null
}

internal enum class EdgeLine(val id: String) {
    SOLID("solid"),
    DOTTED("dotted"),
    THICK("thick"),
    INVISIBLE("invisible"),
}

internal enum class EdgeHead(val id: String) {
    NONE("none"),
    ARROW("arrow"),
    CIRCLE("circle"),
    CROSS("cross"),
}

internal class GraphEdge(
    val from: String,
    val to: String,
    val label: String,
    val line: EdgeLine,
    val start: EdgeHead,
    val end: EdgeHead,
    /** Extra dashes request a longer edge, as in Mermaid. */
    val length: Int,
) {
    var style: DiagramStyle? = null
}

internal class GraphCluster(val id: String, val label: String) {
    var parent: String? = null
    var style: DiagramStyle? = null
}

internal sealed interface Diagram {
    val title: String?
}

internal class GraphDiagram(
    override val title: String?,
    val direction: Direction,
    val nodes: List<GraphNode>,
    val edges: List<GraphEdge>,
    val clusters: List<GraphCluster>,
) : Diagram

internal class Participant(val id: String, var label: String, var actor: Boolean)

internal enum class MessageHead(val id: String) {
    ARROW("arrow"),
    OPEN("open"),
    CROSS("cross"),
    ASYNC("async"),
    NONE("none"),
}

internal enum class NotePlacement(val id: String) {
    LEFT("left"),
    RIGHT("right"),
    OVER("over"),
}

internal enum class BlockKind(val id: String) {
    LOOP("loop"),
    ALT("alt"),
    OPT("opt"),
    PAR("par"),
    CRITICAL("critical"),
    BREAK("break"),
    RECT("rect"),
}

internal sealed interface SequenceItem

internal class SequenceMessage(
    val from: String,
    val to: String,
    val label: String,
    /** Solid or dotted. */
    val line: EdgeLine,
    val head: MessageHead,
    val both: Boolean,
    val activate: Boolean,
    val deactivate: Boolean,
    val number: Double? = null,
) : SequenceItem

internal class SequenceNote(
    val placement: NotePlacement,
    val participants: List<String>,
    val label: String,
) : SequenceItem

internal class SequenceActivation(val active: Boolean, val participant: String) : SequenceItem

internal class SequenceBlock(val kind: BlockKind, val label: String, val color: String? = null) :
    SequenceItem

/** `else`, `and` or `option`. */
internal class SequenceBranch(val kind: String, val label: String) : SequenceItem

internal data object SequenceEnd : SequenceItem

internal class SequenceDiagram(
    override val title: String?,
    val participants: List<Participant>,
    val items: List<SequenceItem>,
) : Diagram

internal class PieSlice(val label: String, val value: Double)

internal class PieDiagram(
    override val title: String?,
    val showData: Boolean,
    val slices: List<PieSlice>,
) : Diagram

internal class DiagramError(message: String) : Exception(message)

internal object DiagramLimits {
    const val SOURCE = 20_000
    const val NODES = 250
    const val EDGES = 500
    const val PARTICIPANTS = 40
    const val ITEMS = 600
    const val SLICES = 40
}

internal enum class DiagramKind {
    FLOWCHART,
    STATE,
    SEQUENCE,
    PIE,
}

/** Fenced code languages rendered as a diagram. */
internal fun isDiagramLanguage(language: String?) = language == "mermaid" || language == "mmd"

/** The language of a fence: the first word of its info string. */
internal fun fenceLanguage(info: String?) = info?.let {
    jsTrim(it).takeWhile { char -> !isJsSpace(char) }
}

private val FLOWCHART_HEADER = Regex("^(?:flowchart|graph)(?:-elk)?$WORD_END")
private val STATE_HEADER = Regex("^statediagram(?:-v2)?$WORD_END")
private val SEQUENCE_HEADER = Regex("^sequencediagram$WORD_END")
private val PIE_HEADER = Regex("^pie$WORD_END")

/** Diagram kinds this renderer draws; others keep their source block. */
internal fun diagramKind(source: String): DiagramKind? {
    val header = statements(source).header?.lowercase() ?: ""
    return when {
        FLOWCHART_HEADER.containsMatchIn(header) -> DiagramKind.FLOWCHART
        STATE_HEADER.containsMatchIn(header) -> DiagramKind.STATE
        SEQUENCE_HEADER.containsMatchIn(header) -> DiagramKind.SEQUENCE
        PIE_HEADER.containsMatchIn(header) -> DiagramKind.PIE
        else -> null
    }
}

internal fun parseDiagram(source: String): Diagram {
    if (source.length > DiagramLimits.SOURCE)
        throw DiagramError("This diagram is too large to draw.")
    return when (diagramKind(source)) {
        DiagramKind.FLOWCHART -> parseFlowchart(source)
        DiagramKind.STATE -> parseState(source)
        DiagramKind.SEQUENCE -> parseSequence(source)
        DiagramKind.PIE -> parsePie(source)
        null -> throw DiagramError("This diagram type is not supported.")
    }
}

private class Statements(val title: String?, val header: String?, val lines: List<String>)

private val LINE_BREAKS = Regex("\r\n?")

/** Remove front matter, directives and comments; return the header and following lines. */
private fun statements(source: String): Statements {
    var lines = source.replace(LINE_BREAKS, "\n").split('\n')
    var title: String? = null
    val first = lines.indexOfFirst { jsTrim(it).isNotEmpty() }
    if (first >= 0 && jsTrim(lines[first]) == "---") {
        val close =
            lines.withIndex().indexOfFirst { (index, line) ->
                index > first && jsTrim(line) == "---"
            }
        if (close > 0) {
            for (line in lines.subList(first + 1, close)) {
                val trimmed = jsTrim(line)
                if (trimmed.startsWith("title:") && trimmed.length > 6)
                    title = unquote(jsTrim(trimmed.substring(6)))
            }

            lines = lines.subList(close + 1, lines.size)
        }
    }

    val kept = mutableListOf<String>()
    for (raw in lines) {
        val line = jsTrim(raw)
        if (line.isEmpty() || line.startsWith("%%")) continue
        kept.add(line)
    }

    val header = kept.removeFirstOrNull()
    return Statements(title, header, kept)
}

private fun unquote(value: String): String {
    val trimmed = jsTrim(value)
    if (
        trimmed.length >= 2 &&
            ((trimmed.startsWith('"') && trimmed.endsWith('"')) ||
                (trimmed.startsWith('\'') && trimmed.endsWith('\'')))
    )
        return trimmed.substring(1, trimmed.length - 1)
    return trimmed
}

/** The first word of a statement and the trimmed rest. Plain scanning keeps parsing linear. */
private fun head(line: String): Pair<String, String> {
    val space = line.indexOfFirst(::isJsSpace)
    return if (space < 0) line to "" else line.substring(0, space) to jsTrim(line.substring(space))
}

/** Text before and after the first colon, both trimmed; `after` is null without one. */
private fun colon(text: String): Pair<String, String?> {
    val index = text.indexOf(':')
    return if (index < 0) jsTrim(text) to null
    else jsTrim(text.substring(0, index)) to jsTrim(text.substring(index + 1))
}

private val ENTITIES =
    mapOf(
        "amp" to "&",
        "lt" to "<",
        "gt" to ">",
        "quot" to "\"",
        "apos" to "'",
        "nbsp" to " ",
        "#39" to "'",
    )

private val BREAK_TAG = Regex("<[Bb][Rr]$SPACE*/?>")
private val TAG = Regex("</?[A-Za-z][^<>]*>")
private val HASH_NUMBER = Regex("(?<!&)#([0-9]+);")
private val HASH_NAME = Regex("(?<!&)#([A-Za-z]+);")
private val ENTITY = Regex("&(#?[A-Za-z0-9_]+);")
private val DECIMAL_REFERENCE = Regex("#[0-9]+")
private val HEX_REFERENCE = Regex("#[xX][0-9A-Fa-f]+")
private val ICON = Regex("(?<![A-Za-z0-9_])fa[bsr]?:fa-[A-Za-z0-9_-]+$SPACE*")
private val BOLD = Regex("\\*\\*($ANY+?)\\*\\*")

/** Mermaid labels allow entity codes, <br> breaks, simple tags and Markdown strings. */
internal fun cleanLabel(value: String): String {
    var label = unquote(value)
    if (label.startsWith('`') && label.endsWith('`') && label.length >= 2)
        label = label.substring(1, label.length - 1)
    label = BREAK_TAG.replace(label, "\n")
    label = label.replace("\\n", "\n")
    label = TAG.replace(label, "")
    label = HASH_NUMBER.replace(label) { codePoint(jsNumber(it.groupValues[1])) }
    label = HASH_NAME.replace(label) { ENTITIES[it.groupValues[1].lowercase()] ?: it.value }
    label =
        ENTITY.replace(label) { match ->
            val name = match.groupValues[1]
            when {
                DECIMAL_REFERENCE.matches(name) -> codePoint(jsNumber(name.substring(1)))
                HEX_REFERENCE.matches(name) -> codePoint(hexNumber(name.substring(2)))
                else -> ENTITIES[name.lowercase()] ?: match.value
            }
        }
    label = ICON.replace(label, "")
    label = BOLD.replace(label, "$1")
    return jsTrim(label.split('\n').joinToString("\n") { jsTrim(it) })
}

/** `String.fromCodePoint`, which rejects values outside Unicode. */
private fun codePoint(value: Double): String {
    if (value != floor(value) || value < 0 || value > 0x10FFFF)
        throw DiagramError("Invalid code point ${jsNumberString(value)}")
    return String(Character.toChars(value.toInt()))
}

private fun hexNumber(digits: String) = BigInteger(digits, 16).toDouble()

private val COLOR =
    Regex(
        "(?:#[0-9A-Fa-f]{3,8}|(?:[Rr][Gg][Bb][Aa]?|[Hh][Ss][Ll][Aa]?)\\([0-9$SPACE_CHARS.,%/-]+\\)|[A-Za-z]{3,20})"
    )
private val IMPORTANT = Regex("![Ii][Mm][Pp][Oo][Rr][Tt][Aa][Nn][Tt]")
private val STYLE_SEPARATOR = Regex("[,;]")

internal fun isDiagramColor(value: String) = COLOR.matches(value)

private fun parseStyle(value: String): DiagramStyle? {
    var style = DiagramStyle()
    for (part in value.split(STYLE_SEPARATOR)) {
        val separator = part.indexOf(':')
        if (separator < 0) continue
        val key = jsTrim(part.substring(0, separator)).lowercase()
        val raw = jsTrim(IMPORTANT.replaceFirst(part.substring(separator + 1), ""))
        if (
            (key == "fill" || key == "background" || key == "background-color") &&
                COLOR.matches(raw)
        ) {
            style = style.copy(fill = raw)
        } else if (key == "stroke" && COLOR.matches(raw)) {
            style = style.copy(stroke = raw)
        } else if (key == "color" && COLOR.matches(raw)) {
            style = style.copy(color = raw)
        } else if (key == "stroke-width") {
            val width = jsParseFloat(raw)
            if (width.isFinite() && width > 0) style = style.copy(strokeWidth = min(width, 6.0))
        } else if (key == "stroke-dasharray") {
            style = style.copy(dashed = raw != "0" && raw != "none")
        }
    }

    return style.takeUnless { it.isEmpty }
}

private val PARENTHESES = Regex("\\(([^)]*)\\)")

// Mermaid's rgb()/rgba() can contain commas, which the property split above must keep together.
private fun splitStyle(value: String) =
    PARENTHESES.replace(value) { "(${it.groupValues[1].replace(',', ' ')})" }

private fun mergeStyle(vararg styles: DiagramStyle?): DiagramStyle? {
    var merged = DiagramStyle()
    for (style in styles) {
        if (style == null) continue
        merged =
            DiagramStyle(
                fill = style.fill ?: merged.fill,
                stroke = style.stroke ?: merged.stroke,
                color = style.color ?: merged.color,
                strokeWidth = style.strokeWidth ?: merged.strokeWidth,
                dashed = style.dashed ?: merged.dashed,
            )
    }
    return merged.takeUnless { it.isEmpty }
}

/** OkHttp applies the WHATWG URL rules to http(s) links, as `new URL(...).href` does. */
private fun safeLink(value: String) = unquote(value).toHttpUrlOrNull()?.toString()

private fun direction(value: String?): Direction =
    when (value?.uppercase()) {
        "LR" -> Direction.LR
        "RL" -> Direction.RL
        "BT" -> Direction.BT
        else -> Direction.TB
    }

private class GraphBuilder {
    val nodes = LinkedHashMap<String, GraphNode>()
    val edges = mutableListOf<GraphEdge>()
    val clusters = LinkedHashMap<String, GraphCluster>()
    val classes = HashMap<String, DiagramStyle>()
    val nodeClasses = HashMap<String, List<String>>()
    val nodeStyles = HashMap<String, DiagramStyle>()
    // `default` or the canonical text of an integer link index.
    val linkStyles = HashMap<String, DiagramStyle>()

    fun node(
        id: String,
        parent: String?,
        shape: NodeShape = NodeShape.RECT,
        label: String? = null,
    ): GraphNode {
        var node = nodes[id]
        if (node == null) {
            if (nodes.size >= DiagramLimits.NODES)
                throw DiagramError("This diagram has too many nodes to draw.")
            node = GraphNode(id, id, shape)
            nodes[id] = node
        }

        if (!parent.isNullOrEmpty() && node.parent.isNullOrEmpty() && !clusters.containsKey(id))
            node.parent = parent
        if (label != null) {
            node.label = label
            node.shape = shape
        }

        return node
    }

    fun edge(edge: GraphEdge) {
        if (edges.size >= DiagramLimits.EDGES)
            throw DiagramError("This diagram has too many links to draw.")
        edges.add(edge)
    }

    fun addClass(ids: String, name: String) {
        for (id in ids.split(',').map(::jsTrim).filter { it.isNotEmpty() }) nodeClasses[id] =
            nodeClasses[id].orEmpty() + name
    }

    fun build(title: String?, dir: Direction): GraphDiagram {
        val nodes = nodes.values.filter { !clusters.containsKey(it.id) }
        for (node in nodes) {
            val classes = nodeClasses[node.id].orEmpty().map { classes[it] }
            val style =
                mergeStyle(this.classes["default"], *classes.toTypedArray(), nodeStyles[node.id])
            if (style != null) node.style = style
        }

        for (cluster in clusters.values) {
            val classes = nodeClasses[cluster.id].orEmpty().map { classes[it] }
            val style = mergeStyle(*classes.toTypedArray(), nodeStyles[cluster.id])
            if (style != null) cluster.style = style
        }

        edges.forEachIndexed { index, edge ->
            val style = mergeStyle(linkStyles["default"], linkStyles[index.toString()])
            if (style != null) edge.style = style
        }

        val known = nodes.map { it.id }.toSet() + clusters.keys
        val edges = edges.filter { known.contains(it.from) && known.contains(it.to) }
        return GraphDiagram(
            title?.takeIf { it.isNotEmpty() },
            dir,
            nodes,
            edges,
            clusters.values.toList(),
        )
    }

    /** Shared styling statements; returns false when the line is something else. */
    fun styling(line: String): Boolean {
        val (keyword, rest) = head(line.removeSuffix(";"))
        if (keyword == "classDef" || keyword == "style" || keyword == "linkStyle") {
            val (target, body) = head(rest)
            if (body.isEmpty()) return false
            val style = parseStyle(splitStyle(body))
            if (keyword == "classDef") {
                for (name in target.split(',')) classes[jsTrim(name)] =
                    mergeStyle(classes[jsTrim(name)], style) ?: DiagramStyle()
            } else if (keyword == "style") {
                if (style != null) nodeStyles[target] = mergeStyle(nodeStyles[target], style)!!
            } else {
                for (index in target.split(',')) {
                    val key = if (jsTrim(index) == "default") "default" else integerKey(index)
                    if (style != null && key != null) linkStyles[key] = style
                }
            }

            return true
        }

        if (keyword == "class") {
            val space = rest.lastIndexOf(' ')
            if (space < 0) return false
            addClass(rest.substring(0, space), rest.substring(space + 1))
            return true
        }

        return keyword == "accTitle" ||
            keyword == "accTitle:" ||
            keyword == "accDescr" ||
            keyword == "accDescr:" ||
            keyword == "interpolate"
    }

    /** `Number(index)` when it is an integer, as the key a link index looks up. */
    private fun integerKey(index: String): String? {
        val value = jsNumber(index)
        if (!value.isFinite() || value != floor(value)) return null
        return if (abs(value) < 1e15) value.toLong().toString() else "big:$value"
    }
}

// Opening delimiters, longest first, with their closing delimiter and shape.
private val SHAPES =
    listOf(
        Triple("(((", ")))", NodeShape.DOUBLE_CIRCLE),
        Triple("((", "))", NodeShape.CIRCLE),
        Triple("([", "])", NodeShape.STADIUM),
        Triple("[[", "]]", NodeShape.SUBROUTINE),
        Triple("[(", ")]", NodeShape.CYLINDER),
        Triple("[/", "/]", NodeShape.PARALLELOGRAM),
        Triple("[\\", "\\]", NodeShape.PARALLELOGRAM_ALT),
        Triple("{{", "}}", NodeShape.HEXAGON),
        Triple("(", ")", NodeShape.ROUND),
        Triple("[", "]", NodeShape.RECT),
        Triple("{", "}", NodeShape.DIAMOND),
        Triple(">", "]", NodeShape.FLAG),
    )

private val NAMED_SHAPES =
    mapOf(
        "rect" to NodeShape.RECT,
        "rectangle" to NodeShape.RECT,
        "proc" to NodeShape.RECT,
        "process" to NodeShape.RECT,
        "rounded" to NodeShape.ROUND,
        "event" to NodeShape.ROUND,
        "stadium" to NodeShape.STADIUM,
        "pill" to NodeShape.STADIUM,
        "terminal" to NodeShape.STADIUM,
        "subproc" to NodeShape.SUBROUTINE,
        "subroutine" to NodeShape.SUBROUTINE,
        "cyl" to NodeShape.CYLINDER,
        "cylinder" to NodeShape.CYLINDER,
        "db" to NodeShape.CYLINDER,
        "database" to NodeShape.CYLINDER,
        "circle" to NodeShape.CIRCLE,
        "circ" to NodeShape.CIRCLE,
        "dbl-circ" to NodeShape.DOUBLE_CIRCLE,
        "double-circle" to NodeShape.DOUBLE_CIRCLE,
        "diam" to NodeShape.DIAMOND,
        "diamond" to NodeShape.DIAMOND,
        "decision" to NodeShape.DIAMOND,
        "question" to NodeShape.DIAMOND,
        "hex" to NodeShape.HEXAGON,
        "hexagon" to NodeShape.HEXAGON,
        "prepare" to NodeShape.HEXAGON,
        "lean-r" to NodeShape.PARALLELOGRAM,
        "in-out" to NodeShape.PARALLELOGRAM,
        "lean-l" to NodeShape.PARALLELOGRAM_ALT,
        "out-in" to NodeShape.PARALLELOGRAM_ALT,
        "trap-b" to NodeShape.TRAPEZOID,
        "trapezoid" to NodeShape.TRAPEZOID,
        "priority" to NodeShape.TRAPEZOID,
        "trap-t" to NodeShape.TRAPEZOID_ALT,
        "inv-trapezoid" to NodeShape.TRAPEZOID_ALT,
        "manual" to NodeShape.TRAPEZOID_ALT,
        "odd" to NodeShape.FLAG,
        "flag" to NodeShape.FLAG,
        "doc" to NodeShape.NOTE,
        "document" to NodeShape.NOTE,
        "notch-rect" to NodeShape.NOTE,
        "card" to NodeShape.NOTE,
        "sm-circ" to NodeShape.START,
        "start" to NodeShape.START,
        "fr-circ" to NodeShape.END,
        "stop" to NodeShape.END,
        "fork" to NodeShape.FORK,
        "join" to NodeShape.FORK,
    )

private fun isIdStop(char: Char) = isJsSpace(char) || "[](){}<>|&;:\"'`,".indexOf(char) >= 0

private val LINK_STARTS = setOf("--", "==", "-.", "~~", "<-", "<=")

private class Scanner(val text: String) {
    var index = 0

    val done
        get() = index >= text.length

    fun skipSpace() {
        while (index < text.length && isJsSpace(text[index])) index++
    }

    fun startsWith(value: String) = text.startsWith(value, index)

    /** A link begins here, so an identifier must end. */
    fun atLink() = text.substring(index, min(index + 2, text.length)) in LINK_STARTS

    fun id(): String {
        val start = index
        while (!done && !isIdStop(text[index]) && !atLink() && !startsWith("@{")) index++
        return text.substring(start, index)
    }

    /** Text up to a closing delimiter; quoted text may contain the delimiter. */
    fun until(close: String): String {
        skipSpace()
        if (startsWith("\"")) {
            val end = text.indexOf('"', index + 1)
            if (end > 0) {
                val quoted = text.substring(index, end + 1)
                index = end + 1
                skipSpace()
                if (startsWith(close)) {
                    index += close.length
                    return quoted
                }

                val rest = text.indexOf(close, index)
                if (rest < 0) throw DiagramError("Missing “$close” after a node label.")
                val result = quoted + text.substring(index, rest)
                index = rest + close.length
                return result
            }
        }

        val end = text.indexOf(close, index)
        if (end < 0) throw DiagramError("Missing “$close” after a node label.")
        val result = text.substring(index, end)
        index = end + close.length
        return result
    }
}

private class NodeRef(val id: String) {
    var shape: NodeShape? = null
    var label: String? = null
    val classes = mutableListOf<String>()
}

private val SHAPE_PROPERTY = Regex("(?<![A-Za-z0-9_])shape$SPACE*:$SPACE*([A-Za-z0-9_-]+)")
private val LABEL_PROPERTY =
    Regex("(?<![A-Za-z0-9_])label$SPACE*:$SPACE*(\"(?:[^\"\\\\]|\\\\$ANY)*\"|'[^']*'|[^,}]+)")

private fun readNode(scanner: Scanner): NodeRef? {
    scanner.skipSpace()
    val id = scanner.id()
    if (id.isEmpty()) return null
    val node = NodeRef(id)

    if (scanner.startsWith("@{")) {
        val end = scanner.text.indexOf('}', scanner.index)
        if (end < 0) throw DiagramError("Missing “}” after node properties.")
        val body = scanner.text.substring(scanner.index + 2, end)
        scanner.index = end + 1
        val shape = SHAPE_PROPERTY.find(body)?.groupValues?.get(1)?.lowercase()
        val label = LABEL_PROPERTY.find(body)?.groupValues?.get(1)
        node.shape = shape?.let { NAMED_SHAPES[it] } ?: NodeShape.RECT
        node.label = if (label == null) id else cleanLabel(label)
    } else {
        for ((open, close, shape) in SHAPES) {
            if (!scanner.startsWith(open)) continue
            scanner.index += open.length
            if (shape == NodeShape.PARALLELOGRAM || shape == NodeShape.PARALLELOGRAM_ALT) {
                // [/text\] and [\text/] are trapezoids; find whichever slash closes the label.
                val slash = scanner.text.indexOf("/]", scanner.index)
                val backslash = scanner.text.indexOf("\\]", scanner.index)
                val ends = listOf(slash, backslash).filter { it >= 0 }
                if (ends.isEmpty()) throw DiagramError("Missing “$close” after a node label.")
                val end = ends.min()
                val closing = scanner.text.substring(end, end + 2)
                node.label = cleanLabel(scanner.text.substring(scanner.index, end))
                scanner.index = end + 2
                node.shape =
                    if (shape == NodeShape.PARALLELOGRAM)
                        if (closing == "/]") NodeShape.PARALLELOGRAM else NodeShape.TRAPEZOID
                    else if (closing == "\\]") NodeShape.PARALLELOGRAM_ALT
                    else NodeShape.TRAPEZOID_ALT
            } else {
                node.label = cleanLabel(scanner.until(close))
                node.shape = shape
            }

            break
        }
    }

    while (scanner.startsWith(":::")) {
        scanner.index += 3
        val name = scanner.id()
        if (name.isNotEmpty()) node.classes.add(name)
    }

    return node
}

private class LinkRef(
    val line: EdgeLine,
    val start: EdgeHead,
    val end: EdgeHead,
    val length: Int,
    var label: String,
)

private val HEADS =
    mapOf(
        '>' to EdgeHead.ARROW,
        '<' to EdgeHead.ARROW,
        'o' to EdgeHead.CIRCLE,
        'x' to EdgeHead.CROSS,
    )

/** Whether a link head at `index` ends there: `o` and `x` are heads only before a gap. */
private fun boundary(text: String, index: Int) =
    index >= text.length || isJsSpace(text[index]) || text[index] == '|'

private fun run(text: String, from: Int, char: Char): Int {
    var end = from
    while (text.getOrNull(end) == char) end++
    return end - from
}

private fun headEnd(text: String, index: Int): Int {
    val char = text.getOrNull(index)
    if (char == '>') return index + 1
    return if ((char == 'o' || char == 'x') && boundary(text, index + 1)) index + 1 else -1
}

/** End of an unlabelled link body starting at `at` (`-->`, `---`, `-.->`, `==>`, `~~~`…), or -1. */
private fun linkBody(text: String, at: Int): Int {
    val char = text.getOrNull(at)
    if (char == '~') {
        val count = run(text, at, '~')
        return if (count >= 3) at + count else -1
    }

    if (char == '-' && text.getOrNull(at + 1) == '.') {
        val close = at + 1 + run(text, at + 1, '.')
        if (text.getOrNull(close) != '-') return -1
        val end = headEnd(text, close + 1)
        return if (end >= 0) end else close + 1
    }

    if (char == '-' || char == '=') {
        val count = run(text, at, char)
        val end = headEnd(text, at + count)
        if (count >= 2 && end >= 0) return end
        return if (count >= 3) at + count else -1
    }

    return -1
}

private class LabelledLink(val label: String, val body: String, val end: Int)

/** `-- text -->`, `== text ==>` and `-. text .->`: the label and the equivalent link body. */
private fun labelledLink(text: String, at: Int): LabelledLink? {
    val open = text.substring(at, min(at + 2, text.length))
    if (
        (open != "--" && open != "==" && open != "-.") ||
            !(text.getOrNull(at + 2)?.let(::isJsSpace) ?: false)
    )
        return null
    for (index in at + 3 until text.length) {
        var end = -1
        var body = ""
        if (open == "-.") {
            if (text[index] == '.' && text[index - 1] != '.') {
                val close = index + run(text, index, '.')
                if (text.getOrNull(close) == '-') {
                    end = headEnd(text, close + 1)
                    end = if (end >= 0) end else close + 1
                    body = "-${text.substring(index, end)}"
                }
            }
        } else if (text[index] == open[0] && text[index - 1] != open[0]) {
            end = linkBody(text, index)
            if (end >= 0) body = text.substring(index, end)
        }

        val label = jsTrim(text.substring(at + 2, index))
        if (end > 0 && label.isNotEmpty()) return LabelledLink(label, body, end)
    }

    return null
}

private fun readLink(scanner: Scanner): LinkRef? {
    scanner.skipSpace()
    val text = scanner.text
    var at = scanner.index
    var startHead: Char? = null
    val first = text.getOrNull(at)
    if (
        first != null &&
            "<ox".indexOf(first) >= 0 &&
            (text.getOrNull(at + 1) == '-' || text.getOrNull(at + 1) == '=')
    ) {
        startHead = first
        at++
    }

    val link: LinkRef
    val end = linkBody(text, at)
    if (end > 0) {
        link = shapeLink(startHead, text.substring(at, end), "")
        scanner.index = end
    } else {
        val labelled = labelledLink(text, at) ?: return null
        link = shapeLink(startHead, labelled.body, cleanLabel(labelled.label))
        scanner.index = labelled.end
    }

    scanner.skipSpace()
    if (scanner.startsWith("|")) {
        scanner.index++
        link.label = cleanLabel(scanner.until("|"))
    }

    return link
}

private fun shapeLink(startHead: Char?, body: String, label: String): LinkRef {
    val last = body.last()
    val end = HEADS[last] ?: EdgeHead.NONE
    val core = if (end == EdgeHead.NONE) body else body.dropLast(1)
    var line = EdgeLine.SOLID
    val length: Int
    if (core.startsWith('~')) {
        line = EdgeLine.INVISIBLE
        length = maxOf(1, core.length - 2)
    } else if (core.contains('.')) {
        line = EdgeLine.DOTTED
        length = maxOf(1, core.count { it == '.' })
    } else if (core.startsWith('=')) {
        line = EdgeLine.THICK
        length = maxOf(1, core.length - (if (end == EdgeHead.NONE) 2 else 1))
    } else {
        length = maxOf(1, core.length - (if (end == EdgeHead.NONE) 2 else 1))
    }

    return LinkRef(
        line = line,
        start = if (startHead != null) HEADS.getValue(startHead) else EdgeHead.NONE,
        end = end,
        length = min(length, 4),
        label = label,
    )
}

private val STYLE_STATEMENT = Regex("^$SPACE*(?:classDef|style|linkStyle)$WORD_END")

private fun splitStatements(lines: List<String>): List<String> {
    val result = mutableListOf<String>()
    for (line in lines) {
        // Semicolons separate statements outside quoted labels and style values.
        val styling by lazy { STYLE_STATEMENT.containsMatchIn(line) }
        var depth = 0
        var quoted = false
        var start = 0
        for (index in line.indices) {
            val char = line[index]
            if (char == '"') {
                quoted = !quoted
            } else if (!quoted && "[({".indexOf(char) >= 0) {
                depth++
            } else if (!quoted && "])}".indexOf(char) >= 0) {
                depth = maxOf(0, depth - 1)
            } else if (!quoted && depth == 0 && char == ';' && !styling) {
                result.add(jsTrim(line.substring(start, index)))
                start = index + 1
            }
        }

        result.add(jsTrim(line.substring(start)))
    }

    return result.filter { it.isNotEmpty() }
}

private val FLOWCHART_DIRECTION = Regex("^[^$SPACE_CHARS]+$SPACE+([A-Za-z0-9_]+)")

private fun parseFlowchart(source: String): GraphDiagram {
    val statements = statements(source)
    val line = statements.header
    // One-line charts put statements after the header: `graph LR; A-->B; B-->C`.
    val separator = line?.indexOf(';') ?: -1
    val header = if (separator < 0) line else line!!.substring(0, separator)
    val lines =
        if (separator < 0) statements.lines
        else listOf(line!!.substring(separator + 1)) + statements.lines
    val builder = GraphBuilder()
    val stack = mutableListOf<String>()
    var subgraphCount = 0

    for (statement in splitStatements(lines)) {
        if (builder.styling(statement)) continue

        val (keyword, rest) = head(statement)
        if (keyword == "subgraph") {
            val spec = rest
            val id: String
            val label: String
            val open = spec.indexOf('[')
            if (
                open > 0 && spec.endsWith(']') && jsTrim(spec.substring(0, open)).none(::isJsSpace)
            ) {
                id = jsTrim(spec.substring(0, open))
                label = cleanLabel(spec.substring(open + 1, spec.length - 1))
            } else if (spec.startsWith('"') || spec.isEmpty()) {
                label = cleanLabel(spec)
                id = "subgraph-${++subgraphCount}"
            } else {
                label = cleanLabel(spec)
                id = if (spec.any(::isJsSpace)) "subgraph-${++subgraphCount}" else spec
            }

            val cluster = GraphCluster(id, label)
            stack.lastOrNull()?.let { cluster.parent = it }
            builder.clusters[id] = cluster
            builder.nodes.remove(id)
            stack.add(id)
            continue
        }

        if (statement == "end") {
            stack.removeLastOrNull()
            continue
        }

        if (keyword == "direction") continue

        if (keyword == "click") {
            val (id, target) = head(rest)
            val (first, second) = head(target)
            val url = if (first == "href") head(second).first else first
            val link = safeLink(url)
            if (link != null) builder.node(id, stack.lastOrNull()).link = link
            continue
        }

        parseChain(statement, builder, stack.lastOrNull())
    }

    val dir = direction(FLOWCHART_DIRECTION.find(header ?: "")?.groupValues?.get(1))
    return builder.build(statements.title, dir)
}

private fun parseChain(line: String, builder: GraphBuilder, parent: String?) {
    val scanner = Scanner(line)
    var previous = readGroup(scanner, builder, parent)
    if (previous.isEmpty()) throw DiagramError("Unexpected “$line”.")

    while (true) {
        scanner.skipSpace()
        if (scanner.done) return
        val link =
            readLink(scanner)
                ?: throw DiagramError("Unexpected “${scanner.text.substring(scanner.index)}”.")
        val next = readGroup(scanner, builder, parent)
        if (next.isEmpty()) throw DiagramError("A link needs a target in “$line”.")
        for (from in previous) {
            for (to in next) {
                builder.edge(
                    GraphEdge(
                        from = from,
                        to = to,
                        label = link.label,
                        line = link.line,
                        start = link.start,
                        end = link.end,
                        length = link.length,
                    )
                )
            }
        }

        previous = next
    }
}

private fun readGroup(scanner: Scanner, builder: GraphBuilder, parent: String?): List<String> {
    val ids = mutableListOf<String>()
    while (true) {
        val ref = readNode(scanner) ?: return ids
        if (builder.clusters.containsKey(ref.id)) {
            ids.add(ref.id)
        } else {
            builder.node(ref.id, parent, ref.shape ?: NodeShape.RECT, ref.label)
            for (name in ref.classes) builder.addClass(ref.id, name)
            ids.add(ref.id)
        }

        scanner.skipSpace()
        if (!scanner.startsWith("&")) return ids
        scanner.index++
    }
}

private val SPACES = Regex("$SPACE+")

private fun parseState(source: String): GraphDiagram {
    val statements = statements(source)
    val lines = statements.lines
    val builder = GraphBuilder()
    val stack = mutableListOf<String>()
    var dir = Direction.TB
    var notes = 0
    val descriptions = LinkedHashMap<String, List<String>>()

    fun scopeKey() = stack.lastOrNull() ?: "root"
    fun endpoint(raw: String, from: Boolean): String {
        val name = jsTrim(raw)
        if (name == "[*]") {
            val id = if (from) "[*]start:${scopeKey()}" else "[*]end:${scopeKey()}"
            builder.node(id, stack.lastOrNull(), if (from) NodeShape.START else NodeShape.END, "")
            return id
        }

        val parts = name.split(":::")
        val id = parts[0]
        val className = parts.getOrNull(1)
        if (!builder.clusters.containsKey(id)) {
            builder.node(id, stack.lastOrNull(), builder.nodes[id]?.shape ?: NodeShape.ROUND)
            if (!className.isNullOrEmpty()) builder.addClass(id, className)
        }

        return id
    }

    var index = 0
    while (index < lines.size) {
        val line = lines[index].removeSuffix(";")
        if (builder.styling(line)) {
            index++
            continue
        }

        val (keyword, rest) = head(line)
        if (keyword == "direction") {
            if (stack.isEmpty()) dir = direction(rest)
            index++
            continue
        }

        if (line == "}") {
            stack.removeLastOrNull()
            index++
            continue
        }

        if (line == "--" || line == "end note") {
            index++
            continue
        }

        if (keyword.lowercase() == "note") {
            val (where, inline) = colon(rest)
            val words = where.split(SPACES)
            val side = words[0].lowercase()
            val target = if (side == "over") words.getOrNull(1) else words.getOrNull(2)
            if (
                target.isNullOrEmpty() ||
                    (side != "over" && words.getOrNull(1)?.lowercase() != "of")
            )
                throw DiagramError("Unexpected “$line”.")
            var text = inline
            if (text == null) {
                val body = mutableListOf<String>()
                while (++index < lines.size && lines[index] != "end note") body.add(lines[index])
                text = body.joinToString("\n")
            }

            val id = "note-${++notes}"
            builder.node(id, stack.lastOrNull(), NodeShape.NOTE, cleanLabel(text))
            val attached = endpoint(target, from = false)
            val left = side == "left"
            builder.edge(
                GraphEdge(
                    from = if (left) id else attached,
                    to = if (left) attached else id,
                    label = "",
                    line = EdgeLine.DOTTED,
                    start = EdgeHead.NONE,
                    end = EdgeHead.NONE,
                    length = 1,
                )
            )
            index++
            continue
        }

        if (keyword == "state") {
            var spec = rest
            var label: String? = null
            if (spec.startsWith('"')) {
                val close = spec.indexOf('"', 1)
                if (close < 0) throw DiagramError("Unexpected “$line”.")
                label = cleanLabel(spec.substring(1, close))
                val (alias, after) = head(jsTrim(spec.substring(close + 1)))
                if (alias != "as") {
                    // `state "Name"` declares a state whose identifier is its name.
                    builder.node(label, stack.lastOrNull(), NodeShape.ROUND, label)
                    index++
                    continue
                }

                spec = after
            }

            val composite = spec.endsWith('{')
            if (composite) spec = jsTrim(spec.dropLast(1))
            val (id, marker) = head(spec)
            if (id.isEmpty()) throw DiagramError("Unexpected “$line”.")
            if (composite) {
                val cluster = GraphCluster(id, label ?: id)
                if (stack.isNotEmpty()) cluster.parent = stack.last()
                builder.clusters[id] = cluster
                builder.nodes.remove(id)
                stack.add(id)
            } else {
                val special =
                    when (marker) {
                        "<<choice>>" -> NodeShape.CHOICE
                        "<<fork>>",
                        "<<join>>" -> NodeShape.FORK
                        else -> null
                    }
                builder.node(
                    id,
                    stack.lastOrNull(),
                    special ?: NodeShape.ROUND,
                    if (special != null) "" else (label ?: id),
                )
            }

            index++
            continue
        }

        val arrow = line.indexOf("-->")
        if (arrow > 0) {
            val (target, text) = colon(line.substring(arrow + 3))
            val origin = jsTrim(line.substring(0, arrow))
            if (
                origin.isEmpty() ||
                    target.isEmpty() ||
                    origin.any(::isJsSpace) ||
                    target.any(::isJsSpace)
            )
                throw DiagramError("Unexpected “$line”.")
            val from = endpoint(origin, from = true)
            val to = endpoint(target, from = false)
            builder.edge(
                GraphEdge(
                    from = from,
                    to = to,
                    label = if (!text.isNullOrEmpty()) cleanLabel(text) else "",
                    line = EdgeLine.SOLID,
                    start = EdgeHead.NONE,
                    end = EdgeHead.ARROW,
                    length = 1,
                )
            )
            index++
            continue
        }

        val (name, description) = colon(line)
        if (
            name.isNotEmpty() &&
                name.none(::isJsSpace) &&
                (description == null || description.isNotEmpty())
        ) {
            val id = endpoint(name, from = false)
            if (!description.isNullOrEmpty())
                descriptions[id] = descriptions[id].orEmpty() + cleanLabel(description)
            index++
            continue
        }

        throw DiagramError("Unexpected “$line”.")
    }

    for ((id, labels) in descriptions) builder.nodes[id]?.label = labels.joinToString("\n")

    return builder.build(statements.title, dir)
}

// Longest first at any position. Arrows ending in `x` are tried last: names may contain `-x`.
private val ARROWS = listOf("<<-->>", "<<->>", "-->>", "->>", "--)", "-)", "-->", "->")
private val CROSSES = listOf("--x", "-x")

/** The first arrow before any message text: its index and token. */
private fun findArrow(line: String): Pair<Int, String>? {
    val before = if (line.contains(':')) line.substring(0, line.indexOf(':')) else line
    for (tokens in listOf(ARROWS, CROSSES)) {
        for (index in 1 until before.length) {
            val token = tokens.find { before.startsWith(it, index) }
            if (token != null) return index to token
        }
    }

    return null
}

private val SKIPPED_SEQUENCE_KEYWORDS =
    setOf(
        "destroy",
        "link",
        "links",
        "properties",
        "details",
        "accTitle",
        "accTitle:",
        "accDescr",
        "accDescr:",
    )
private val BLOCK_KEYWORDS =
    setOf("loop", "alt", "opt", "par", "par_over", "critical", "break", "rect")
private val ALL_SPACE = Regex("$SPACE")

private fun parseSequence(source: String): SequenceDiagram {
    val statements = statements(source)
    var title = statements.title
    val participants = LinkedHashMap<String, Participant>()
    val items = mutableListOf<SequenceItem>()
    var autonumber: Double? = null
    // Boxes group participant columns; they open and close like blocks but draw nothing here.
    val open = mutableListOf<Boolean>()

    fun participant(raw: String): String {
        val id = jsTrim(raw)
        var existing = participants[id]
        if (existing == null) {
            if (participants.size >= DiagramLimits.PARTICIPANTS)
                throw DiagramError("This diagram has too many participants to draw.")
            existing = Participant(id, cleanLabel(id), false)
            participants[id] = existing
        }

        return existing.id
    }

    fun push(item: SequenceItem) {
        if (items.size >= DiagramLimits.ITEMS)
            throw DiagramError("This diagram has too many steps to draw.")
        items.add(item)
    }

    for (raw in statements.lines) {
        val line = raw.removeSuffix(";")
        val (keyword, rest) = head(line)
        val lower = keyword.lowercase()
        if (lower == "title" || lower.startsWith("title:")) {
            title = cleanLabel(jsTrim(line.substring(5)).removePrefix(":"))
            continue
        }

        if (keyword == "autonumber") {
            autonumber =
                if (rest == "off") null
                else jsParseInt(rest).takeUnless { it.isNaN() || it == 0.0 } ?: 1.0
            continue
        }

        val declaration = if (keyword == "create") head(rest) else keyword to rest
        if (declaration.first == "participant" || declaration.first == "actor") {
            val spec = declaration.second
            val alias = spec.indexOf(" as ")
            val declared = if (alias < 0) spec else spec.substring(0, alias)
            val properties = declared.indexOf("@{")
            val id = jsTrim(if (properties < 0) declared else declared.substring(0, properties))
            val label = if (alias < 0) cleanLabel(id) else cleanLabel(spec.substring(alias + 4))
            val actor =
                declaration.first == "actor" ||
                    (properties >= 0 &&
                        declared
                            .substring(properties)
                            .replace(ALL_SPACE, "")
                            .contains("\"type\":\"actor\""))
            participant(id)
            val known = participants.getValue(id)
            known.label = label
            known.actor = actor
            continue
        }

        if (keyword in SKIPPED_SEQUENCE_KEYWORDS) continue

        if ((keyword == "activate" || keyword == "deactivate") && rest.isNotEmpty()) {
            push(SequenceActivation(keyword == "activate", participant(rest)))
            continue
        }

        if (lower == "note") {
            val (where, text) = colon(rest)
            val (side, targets) = head(where)
            val placement = side.lowercase()
            val list =
                if (placement == "over") targets
                else if (head(targets).first.lowercase() == "of") head(targets).second else ""
            if (
                (placement == "left" || placement == "right" || placement == "over") &&
                    list.isNotEmpty() &&
                    text != null
            ) {
                val ids = list.split(',').map(::participant)
                push(
                    SequenceNote(
                        placement = NotePlacement.entries.first { it.id == placement },
                        participants = ids.take(2),
                        label = cleanLabel(text),
                    )
                )
                continue
            }
        }

        if (keyword == "box") {
            open.add(false)
            continue
        }

        if (keyword in BLOCK_KEYWORDS) {
            val kind =
                if (keyword == "par_over") BlockKind.PAR
                else BlockKind.entries.first { it.id == keyword }
            val color = rest.takeIf {
                kind == BlockKind.RECT && COLOR.matches(it) && it != "transparent"
            }
            val block =
                SequenceBlock(kind, if (kind == BlockKind.RECT) "" else cleanLabel(rest), color)
            open.add(true)
            push(block)
            continue
        }

        if ((keyword == "else" || keyword == "and" || keyword == "option") && open.contains(true)) {
            push(SequenceBranch(keyword, cleanLabel(rest)))
            continue
        }

        if (line == "end") {
            if (open.removeLastOrNull() == true) push(SequenceEnd)
            continue
        }

        val arrow = findArrow(line)
        if (arrow != null) {
            val (index, token) = arrow
            var (target, text) = colon(line.substring(index + token.length))
            val mark = if (target.startsWith('+') || target.startsWith('-')) target[0] else null
            if (mark != null) target = jsTrim(target.substring(1))
            val origin = jsTrim(line.substring(0, index))
            if (origin.isEmpty() || target.isEmpty()) throw DiagramError("Unexpected “$line”.")
            val dotted = token.startsWith("--") || token.startsWith("<<--")
            val head =
                when {
                    token.endsWith(">>") -> MessageHead.ARROW
                    token.endsWith("x") -> MessageHead.CROSS
                    token.endsWith(")") -> MessageHead.ASYNC
                    else -> MessageHead.NONE
                }
            val from = participant(origin)
            val to = participant(target)
            val number = autonumber
            if (autonumber != null) autonumber += 1
            push(
                SequenceMessage(
                    from = from,
                    to = to,
                    label = cleanLabel(text ?: ""),
                    line = if (dotted) EdgeLine.DOTTED else EdgeLine.SOLID,
                    head = head,
                    both = token.startsWith("<<"),
                    activate = mark == '+',
                    deactivate = mark == '-',
                    number = number,
                )
            )
            continue
        }

        throw DiagramError("Unexpected “$line”.")
    }

    for (block in open) {
        if (block) items.add(SequenceEnd)
    }

    return SequenceDiagram(
        title?.takeIf { it.isNotEmpty() },
        participants.values.toList(),
        items,
    )
}

private val PIE_VALUE = Regex("-?[0-9.]+(?:[eE][+-]?[0-9]+)?")
private val PIE_ACCESSIBILITY = Regex("^(?:accTitle|accDescr)$WORD_END")

private fun parsePie(source: String): PieDiagram {
    val statements = statements(source)
    var title = statements.title
    val (_, options) = head(statements.header ?: "")
    var showData = options.split(SPACES).contains("showData")
    val inline = options.indexOf("title ")
    if (inline >= 0) title = cleanLabel(options.substring(inline + 6))
    val slices = mutableListOf<PieSlice>()
    for (line in statements.lines) {
        val (keyword, rest) = head(line)
        if (keyword == "title") {
            title = cleanLabel(rest)
            continue
        }

        if (line == "showData") {
            showData = true
            continue
        }

        val separator = line.lastIndexOf(':')
        val value = if (separator > 0) jsTrim(line.substring(separator + 1)) else ""
        if (PIE_VALUE.matches(value)) {
            val number = jsNumber(value)
            if (!number.isFinite() || number < 0)
                throw DiagramError("Pie values must be positive numbers.")
            if (slices.size >= DiagramLimits.SLICES)
                throw DiagramError("This chart has too many slices to draw.")
            slices.add(PieSlice(cleanLabel(line.substring(0, separator)), number))
            continue
        }

        if (PIE_ACCESSIBILITY.containsMatchIn(line)) continue
        throw DiagramError("Unexpected “$line”.")
    }

    if (slices.none { it.value > 0 }) throw DiagramError("This chart has no values to draw.")
    return PieDiagram(title?.takeIf { it.isNotEmpty() }, showData, slices)
}

// JavaScript semantics the shared corpus depends on. Android's ICU regular expressions treat
// \s, \w, \d, \b and case folding as Unicode, so patterns spell out JavaScript's classes.

/** JavaScript's WhiteSpace and LineTerminator characters, as matched by `\s` and `trim()`. */
internal const val SPACE_CHARS =
    "\\t\\n\\u000B\\f\\r \\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF"
internal const val SPACE = "[$SPACE_CHARS]"
/** JavaScript's `.`, which stops at line terminators. */
private const val ANY = "[^\\n\\r\\u2028\\u2029]"
/** JavaScript's `\b` after a word character. */
private const val WORD_END = "(?![A-Za-z0-9_])"

internal fun isJsSpace(char: Char) =
    when (char.code) {
        0x09,
        0x0A,
        0x0B,
        0x0C,
        0x0D,
        0x20,
        0xA0,
        0x1680,
        0x2028,
        0x2029,
        0x202F,
        0x205F,
        0x3000,
        0xFEFF -> true
        else -> char.code in 0x2000..0x200A
    }

internal fun jsTrim(value: String) = value.trim(::isJsSpace)

/** `Math.round`: halves round toward positive infinity. */
internal fun jsRound(value: Double): Double {
    if (!value.isFinite()) return value
    val base = floor(value)
    return if (value - base >= 0.5) base + 1 else base
}

private val DECIMAL_LITERAL = Regex("[+-]?(?:[0-9]+(?:\\.[0-9]*)?|\\.[0-9]+)(?:[eE][+-]?[0-9]+)?")
private val FLOAT_PREFIX =
    Regex("^[+-]?(?:Infinity|(?:[0-9]+(?:\\.[0-9]*)?|\\.[0-9]+)(?:[eE][+-]?[0-9]+)?)")
private val INTEGER_PREFIX = Regex("^[+-]?[0-9]+")

/** `Number(value)`. */
internal fun jsNumber(value: String): Double {
    val text = jsTrim(value)
    if (text.isEmpty()) return 0.0
    val radix =
        when (text.take(2)) {
            "0x",
            "0X" -> 16
            "0o",
            "0O" -> 8
            "0b",
            "0B" -> 2
            else -> 0
        }
    if (radix != 0)
        return runCatching { BigInteger(text.substring(2), radix).toDouble() }
            .getOrDefault(Double.NaN)
    return when {
        text == "Infinity" || text == "+Infinity" -> Double.POSITIVE_INFINITY
        text == "-Infinity" -> Double.NEGATIVE_INFINITY
        DECIMAL_LITERAL.matches(text) -> text.toDouble()
        else -> Double.NaN
    }
}

/** `Number.parseFloat(value)`: the longest decimal prefix. */
internal fun jsParseFloat(value: String): Double {
    val prefix = FLOAT_PREFIX.find(value.trimStart(::isJsSpace))?.value ?: return Double.NaN
    return when (prefix) {
        "Infinity",
        "+Infinity" -> Double.POSITIVE_INFINITY
        "-Infinity" -> Double.NEGATIVE_INFINITY
        else -> prefix.toDouble()
    }
}

/** `Number.parseInt(value, 10)`. */
internal fun jsParseInt(value: String): Double =
    INTEGER_PREFIX.find(value.trimStart(::isJsSpace))?.value?.toDouble() ?: Double.NaN

/** `String(value)` for the finite numbers a diagram prints. */
internal fun jsNumberString(value: Double): String {
    if (value.isNaN()) return "NaN"
    if (value.isInfinite()) return if (value > 0) "Infinity" else "-Infinity"
    if (value == 0.0) return "0"
    if (value == floor(value) && abs(value) < 1e21) return BigDecimal(value).toPlainString()
    val decimal = BigDecimal(value.toString())
    if (abs(value) >= 1e-6 && abs(value) < 1e21) return decimal.stripTrailingZeros().toPlainString()
    val digits = decimal.unscaledValue().abs().toString().trimEnd('0').ifEmpty { "0" }
    val exponent = decimal.precision() - decimal.scale() - 1
    val mantissa = if (digits.length > 1) "${digits[0]}.${digits.substring(1)}" else digits
    val sign = if (value < 0) "-" else ""
    return "$sign${mantissa}e${if (exponent >= 0) "+" else "-"}${abs(exponent)}"
}

/** `value.toLocaleString('en-US')`: grouped thousands and at most three decimals. */
internal fun jsLocaleNumber(value: Double): String {
    if (value.isNaN()) return "NaN"
    if (value.isInfinite()) return if (value > 0) "∞" else "-∞"
    val negative = value < 0 || (value == 0.0 && 1 / value < 0)
    val decimal =
        BigDecimal(abs(value).toString()).setScale(3, RoundingMode.HALF_UP).stripTrailingZeros()
    val plain = decimal.toPlainString()
    val whole = plain.substringBefore('.')
    val fraction = plain.substringAfter('.', "")
    val grouped = whole.reversed().chunked(3).joinToString(",").reversed()
    return (if (negative) "-" else "") + grouped + (if (fraction.isEmpty()) "" else ".$fraction")
}
