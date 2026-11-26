package build.cairn.app.ui

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

private val SEQUENCE_NUMBER = Regex("[+-]?(?:[0-9]+(?:\\.[0-9]{1,2})?|\\.[0-9]{1,2})")
private const val MAX_SEQUENCE_NUMBER = 9007199254740991L

/** Numbering uses hundredths to avoid accumulating floating-point rounding errors. */
private fun sequenceNumber(value: String): Long {
    val parts = value.removePrefix("-").removePrefix("+").split('.')
    val digits =
        (parts[0] + parts.getOrElse(1) { "" }.padEnd(2, '0')).trimStart('0').ifEmpty { "0" }
    val cents =
        digits.toLongOrNull()?.takeIf { it <= MAX_SEQUENCE_NUMBER }
            ?: throw DiagramError("The sequence number is too large.")
    return if (value.startsWith('-')) -cents else cents
}

private val SEQUENCE_SEPARATORS = Regex("(?:&?#|&)\\w+;|;")

/** Sequence semicolons separate steps; entity terminators stay in their labels. */
private fun sequenceStatements(lines: List<String>): List<String> {
    val result = mutableListOf<String>()
    for (line in lines) {
        var start = 0
        for (match in SEQUENCE_SEPARATORS.findAll(line)) {
            if (match.value != ";") continue
            val part = jsTrim(line.substring(start, match.range.first))
            if (part.isNotEmpty()) result.add(part)
            start = match.range.last + 1
        }
        val last = jsTrim(line.substring(start))
        if (last.isNotEmpty()) result.add(last)
    }
    return result
}

internal fun parseSequence(source: String): SequenceDiagram {
    val statements = statements(source)
    val inline = jsTrim(statements.header.orEmpty().drop("sequenceDiagram".length))
    if (inline.isNotEmpty() && !inline.startsWith(';'))
        throw DiagramError("The sequence header must end before its first step.")
    var title = statements.title
    val participants = LinkedHashMap<String, Participant>()
    val items = mutableListOf<SequenceItem>()
    var autonumber: Long? = null
    var increment = 100L
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

    for (line in sequenceStatements(listOf(inline) + statements.lines)) {
        val (keyword, rest) = head(line)
        val lower = keyword.lowercase()
        if (lower == "title" || lower.startsWith("title:")) {
            title = cleanLabel(jsTrim(line.substring(5)).removePrefix(":"))
            continue
        }

        if (keyword == "autonumber") {
            if (rest == "off") autonumber = null
            else {
                val values = if (rest.isEmpty()) listOf("1") else rest.split(SPACES)
                if (values.size > 2 || values.any { !SEQUENCE_NUMBER.matches(it) })
                    throw DiagramError(
                        "Sequence numbering supports a start and increment with at most two decimal places."
                    )
                autonumber = sequenceNumber(values[0])
                increment = sequenceNumber(values.getOrElse(1) { "1" })
            }
            continue
        }

        if (keyword == "create" || keyword == "destroy")
            throw DiagramError("Participant creation and destruction are not supported.")

        if (keyword == "participant" || keyword == "actor") {
            val spec = rest
            val alias = spec.indexOf(" as ")
            val declared = if (alias < 0) spec else spec.substring(0, alias)
            val properties = declared.indexOf("@{")
            val id = jsTrim(if (properties < 0) declared else declared.substring(0, properties))
            val label = if (alias < 0) cleanLabel(id) else cleanLabel(spec.substring(alias + 4))
            val actor =
                keyword == "actor" ||
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

        if (keyword == "box") throw DiagramError("Sequence participant groups are not supported.")

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
            val number = autonumber?.toDouble()?.div(100)
            if (autonumber != null) {
                autonumber += increment
                if (autonumber > MAX_SEQUENCE_NUMBER || autonumber < -MAX_SEQUENCE_NUMBER)
                    throw DiagramError("The sequence number is too large.")
            }
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

internal fun parsePie(source: String): PieDiagram {
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
