package build.cairn.app.ui

import kotlin.math.min

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

private fun run(text: String, from: Int, char: Char): Int {
    var end = from
    while (text.getOrNull(end) == char) end++
    return end - from
}

private fun headEnd(text: String, index: Int): Int {
    val char = text.getOrNull(index)
    if (char == '>') return index + 1
    return if ((char == 'o' || char == 'x')) index + 1 else -1
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
        if (open == "-.") {
            if (text[index] == '.' && text[index - 1] != '.') {
                val close = index + run(text, index, '.')
                if (text.getOrNull(close) == '-') {
                    end = headEnd(text, close + 1)
                    end = if (end >= 0) end else close + 1
                }
            }
        } else if (text[index] == open[0] && text[index - 1] != open[0]) {
            end = linkBody(text, index)
        }

        if (end <= 0) continue
        val label = jsTrim(text.substring(at + 2, index))
        if (label.isNotEmpty()) {
            val body = (if (open == "-.") "-" else "") + text.substring(index, end)
            return LabelledLink(label, body, end)
        }
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

private val STYLE_PROPERTY = Regex("^[A-Za-z][A-Za-z-]*$SPACE*:")

private fun splitStatements(lines: List<String>): List<String> {
    val result = mutableListOf<String>()
    for (line in lines) {
        val parts = mutableListOf<String>()
        var depth = 0
        var quoted = false
        var start = 0
        for (index in line.indices) {
            val char = line[index]
            if (char == '"') quoted = !quoted
            else if (!quoted && "[({".indexOf(char) >= 0) depth++
            else if (!quoted && "])}".indexOf(char) >= 0) depth = maxOf(0, depth - 1)
            else if (!quoted && depth == 0 && char == ';') {
                parts.add(jsTrim(line.substring(start, index)))
                start = index + 1
            }
        }
        parts.add(jsTrim(line.substring(start)))

        // Preserve semicolon-separated CSS properties, but never absorb the next graph statement.
        var styling = false
        for (part in parts) {
            if (part.isEmpty()) continue
            if (styling && STYLE_PROPERTY.containsMatchIn(part)) {
                result[result.lastIndex] += ";$part"
            } else {
                result.add(part)
                styling = STYLE_STATEMENT.containsMatchIn(part)
            }
        }
    }
    return result
}

private val FLOWCHART_DIRECTION = Regex("^[^$SPACE_CHARS]+$SPACE+([A-Za-z0-9_]+)")

internal fun parseFlowchart(source: String): GraphDiagram {
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

        if (keyword == "direction") {
            val parent =
                stack.lastOrNull()
                    ?: throw DiagramError("Set the flowchart direction in its header.")
            builder.clusterDirections[parent] = direction(rest)
            continue
        }

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
    return builder.build(statements.title, dir, true)
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

internal val SPACES = Regex("$SPACE+")

/** Endpoint class suffixes are not the single-colon delimiter before state label text. */
private fun stateLabel(text: String): Pair<String, String?> {
    var index = 0
    while (index < text.length) {
        if (text.startsWith(":::", index)) {
            index += 3
            continue
        }
        if (text[index] == ':')
            return jsTrim(text.substring(0, index)) to jsTrim(text.substring(index + 1))
        index++
    }
    return jsTrim(text) to null
}

internal fun parseState(source: String): GraphDiagram {
    val statements = statements(source)
    val lines = statements.lines
    val builder = GraphBuilder()
    val stack = mutableListOf<String>()
    var dir = Direction.TB
    var notes = 0
    val descriptions = LinkedHashMap<String, List<String>>()

    fun scopeKey() = if (stack.isEmpty()) "document" else "state:${stack.last()}"
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
            else builder.clusterDirections[stack.last()] = direction(rest)
            index++
            continue
        }

        if (line == "}") {
            stack.removeLastOrNull()
            index++
            continue
        }

        if (line == "--") throw DiagramError("Concurrent state regions are not supported.")

        if (line == "end note") {
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

            if (side != "left" && side != "right")
                throw DiagramError("Only left and right state notes are supported.")
            val id = "\u0000note:${++notes}"
            val note = builder.node(id, stack.lastOrNull(), NodeShape.NOTE, cleanLabel(text))
            val attached = endpoint(target, from = false)
            note.annotation =
                StateAnnotation(
                    attached,
                    if (side == "left") NotePlacement.LEFT else NotePlacement.RIGHT,
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
            val (target, text) = stateLabel(line.substring(arrow + 3))
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

        val (name, description) = stateLabel(line)
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

    for ((id, labels) in descriptions) {
        val cluster = builder.clusters[id]
        if (cluster != null) cluster.label = labels.joinToString("\n")
        else builder.nodes[id]?.label = labels.joinToString("\n")
    }

    return builder.build(statements.title, dir)
}
