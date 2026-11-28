package build.cairn.app.ui

import java.math.BigDecimal
import java.math.BigInteger
import java.math.RoundingMode
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.min
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull

internal class Statements(val title: String?, val header: String?, val lines: List<String>)

private val LINE_BREAKS = Regex("\r\n?")

/** Remove front matter, directives and comments; return the header and following lines. */
internal fun statements(source: String): Statements {
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

internal fun unquote(value: String): String {
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
internal fun head(line: String): Pair<String, String> {
    val space = line.indexOfFirst(::isJsSpace)
    return if (space < 0) line to "" else line.substring(0, space) to jsTrim(line.substring(space))
}

/** Text before and after the first colon, both trimmed; `after` is null without one. */
internal fun colon(text: String): Pair<String, String?> {
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

internal val COLOR =
    Regex(
        "(?:#[0-9A-Fa-f]{3,8}|(?:[Rr][Gg][Bb][Aa]?|[Hh][Ss][Ll][Aa]?)\\([0-9$SPACE_CHARS.,%/-]+\\)|[A-Za-z]{3,20})"
    )
private val IMPORTANT = Regex("![Ii][Mm][Pp][Oo][Rr][Tt][Aa][Nn][Tt]")

internal fun isDiagramColor(value: String) = COLOR.matches(value)

internal fun parseStyle(value: String): DiagramStyle? {
    var style = DiagramStyle()
    for (part in splitStyle(value)) {
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

// Split properties outside color functions, preserving valid legacy and modern CSS syntax.
internal fun splitStyle(value: String): List<String> {
    val parts = mutableListOf<String>()
    var start = 0
    var depth = 0
    for ((index, character) in value.withIndex()) {
        when {
            character == '(' -> depth++
            character == ')' -> depth = maxOf(0, depth - 1)
            depth == 0 && (character == ',' || character == ';') -> {
                parts.add(value.substring(start, index))
                start = index + 1
            }
        }
    }
    parts.add(value.substring(start))
    return parts
}

internal fun mergeStyle(vararg styles: DiagramStyle?): DiagramStyle? {
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
internal fun safeLink(value: String) = unquote(value).toHttpUrlOrNull()?.toString()

internal fun direction(value: String?): Direction =
    when (value?.uppercase()) {
        "LR" -> Direction.LR
        "RL" -> Direction.RL
        "BT" -> Direction.BT
        else -> Direction.TB
    }

internal class GraphBuilder {
    val clusterDirections = LinkedHashMap<String, Direction>()
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

    fun build(
        title: String?,
        dir: Direction,
        inheritExternalDirection: Boolean = false,
    ): GraphDiagram {
        // Reusing a cluster ID can reparent an existing ancestor. Reject cycles before layout.
        if (clusters.size > DiagramLimits.CLUSTERS)
            throw DiagramError("This diagram has too many subgraphs to draw.")
        val checked = mutableMapOf<String, Int>()
        for (id in clusters.keys) {
            val ancestors = mutableSetOf<String>()
            var current: String? = id
            while (current != null && current !in checked) {
                if (!ancestors.add(current))
                    throw DiagramError("Subgraphs cannot contain themselves.")
                if (ancestors.size > DiagramLimits.CLUSTER_DEPTH)
                    throw DiagramError("This diagram nests subgraphs too deeply to draw.")
                current = clusters[current]?.parent
            }
            var depth = if (current != null) checked[current] ?: 0 else 0
            for (ancestor in ancestors.toList().asReversed()) {
                depth++
                if (depth > DiagramLimits.CLUSTER_DEPTH)
                    throw DiagramError("This diagram nests subgraphs too deeply to draw.")
                checked[ancestor] = depth
            }
        }

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

        fun inGroup(key: String, group: String): Boolean {
            if (key == group) return true
            var parent = this.nodes[key]?.parent ?: clusters[key]?.parent
            while (parent != null) {
                if (parent == group) return true
                parent = clusters[parent]?.parent
            }
            return false
        }

        for ((id, requested) in clusterDirections) {
            if (requested == dir) continue
            fun inside(key: String) = inGroup(key, id)
            // Mermaid inherits the parent direction when a member node has an external link.
            val externalMemberLink =
                this.edges.any { edge ->
                    (this.nodes.containsKey(edge.from) &&
                        !clusters.containsKey(edge.from) &&
                        inside(edge.from) &&
                        !inside(edge.to)) ||
                        (this.nodes.containsKey(edge.to) &&
                            !clusters.containsKey(edge.to) &&
                            inside(edge.to) &&
                            !inside(edge.from))
                }
            if (!inheritExternalDirection || !externalMemberLink)
                throw DiagramError("Subgraphs with a different local direction are not supported.")
        }

        if (this.edges.any { it.from == it.to && clusters.containsKey(it.from) })
            throw DiagramError(
                "Self-transitions on composite states or subgraphs are not supported."
            )

        if (
            this.edges.any { edge ->
                (clusters.containsKey(edge.from) && inGroup(edge.to, edge.from)) ||
                    (clusters.containsKey(edge.to) && inGroup(edge.from, edge.to))
            }
        )
            throw DiagramError("Links between a group and its own descendants are not supported.")

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
            val style = parseStyle(body)
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

// JavaScript semantics the shared corpus depends on. Android's ICU regular expressions treat
// \s, \w, \d, \b and case folding as Unicode, so patterns spell out JavaScript's classes.

/** JavaScript's WhiteSpace and LineTerminator characters, as matched by `\s` and `trim()`. */
internal const val SPACE_CHARS =
    "\\t\\n\\u000B\\f\\r \\u00A0\\u1680\\u2000-\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF"
internal const val SPACE = "[$SPACE_CHARS]"
/** JavaScript's `.`, which stops at line terminators. */
internal const val ANY = "[^\\n\\r\\u2028\\u2029]"
/** JavaScript's `\b` after a word character. */
internal const val WORD_END = "(?![A-Za-z0-9_])"

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
