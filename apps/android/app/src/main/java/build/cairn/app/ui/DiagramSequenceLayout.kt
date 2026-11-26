package build.cairn.app.ui

import kotlin.math.ceil
import kotlin.math.max
import kotlin.math.min

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

    val depths = IntArray(diagram.participants.size)
    val peakDepths = IntArray(diagram.participants.size)
    fun changeDepth(participant: String, delta: Int) {
        val column = index.getValue(participant)
        depths[column] = max(0, depths[column] + delta)
        peakDepths[column] = max(peakDepths[column], depths[column])
    }
    for (item in diagram.items) when (item) {
        is SequenceActivation -> changeDepth(item.participant, if (item.active) 1 else -1)
        is SequenceMessage -> {
            if (item.activate) changeDepth(item.to, 1)
            if (item.deactivate) changeDepth(item.from, -1)
        }
        else -> Unit
    }
    val activationReach = peakDepths.map { if (it > 0) (it - 1) * 4 + 5.5 else 0.0 }

    // Column gaps grow until every message and note fits between its participants.
    val gaps =
        heads
            .drop(1)
            .mapIndexed { position, head ->
                max(heads[position].w / 2, activationReach[position]) + head.w / 2 + ACTOR_GAP
            }
            .toMutableList()
    var leftRoom = 0.0
    var rightRoom = max(0.0, activationReach.last() - heads.last().w / 2)
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
                    val numbered =
                        if (item.number == null) 0.0
                        else sequenceBadgeWidth(item.number, measure) + 10
                    if (from == to) {
                        if (from + 1 < heads.size)
                            need(from, from + 1, text.width + 64 + numbered + activationReach[from])
                        else
                            rightRoom =
                                max(rightRoom, text.width + 64 + numbered + activationReach[from])
                    } else {
                        need(
                            from,
                            to,
                            text.width +
                                36 +
                                numbered +
                                activationReach[from] +
                                activationReach[to],
                        )
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
    val framePadding = 14.0
    fun frameStartX(frame: Frame): Double {
        val columns =
            if (frame.columns.isNotEmpty()) frame.columns.toList() else centers.indices.toList()
        val columnLeft = minimum(columns.map { centers[it] }) - 52 + frame.depth * 6
        return max(MARGIN / 2 + frame.depth * 6, min(columnLeft, frame.low - framePadding))
    }
    fun frameBounds(frame: Frame): Pair<Double, Double> {
        val columns =
            if (frame.columns.isNotEmpty()) frame.columns.toList() else centers.indices.toList()
        val left = frameStartX(frame)
        val columnRight = maximum(columns.map { centers[it] }) + 52 - frame.depth * 6
        return Pair(left, max(max(columnRight, left + 140), frame.high + framePadding))
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
        val x = centers[column] + active[column].size * 4
        touch(column)
        extend(x - 5.5, x + 5.5)
        active[column].add(at)
    }

    fun deactivate(column: Int, at: Double) {
        val start = active[column].removeLastOrNull()
        if (start != null) {
            val x = centers[column] + active[column].size * 4
            touch(column)
            extend(x - 5.5, x + 5.5)
            activations.add(Activation(column, start, at, active[column].size))
        }
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
                    val badgeWidth =
                        if (item.number == null) 0.0 else sequenceBadgeWidth(item.number, measure)
                    val labelX = x0 + 12 + (if (item.number == null) 0.0 else badgeWidth + 10)
                    rows.addAll(messageText(block, labelX, labelY, TextAnchor.START))
                    extend(x0, max(x0 + 44, labelX + block.width))
                    if (item.number != null)
                        rows.addAll(badge(item.number, x0 + 12 + badgeWidth / 2, labelY, measure))
                    y = loopBottom + 16
                } else {
                    val labelY = y + block.height / 2
                    y += max(block.height, 4.0) + (if (item.number == null) 6 else 14)
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
                    val labelX = middle + (if (item.number == null) 0.0 else 11.0)
                    val badgeX = x0 + direction * 14
                    val badgeHalfWidth =
                        if (item.number == null) 0.0
                        else sequenceBadgeWidth(item.number, measure) / 2
                    rows.addAll(messageText(block, labelX, labelY, TextAnchor.MIDDLE))
                    extend(
                        minOf(x0, x1, labelX - block.width / 2, badgeX - badgeHalfWidth),
                        maxOf(x0, x1, labelX + block.width / 2, badgeX + badgeHalfWidth),
                    )
                    if (item.number != null) rows.addAll(badge(item.number, badgeX, y, measure))
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
                    // Include the drawn minimum width and unused columns before enclosing this
                    // child.
                    val (frameLeft, frameRight) = frameBounds(frame)
                    frame.low = min(frame.low, frameLeft + framePadding)
                    frame.high = max(frame.high, frameRight - framePadding)
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
        val (left, frameEnd) = frameBounds(frame)
        widest = max(widest, frameEnd + MARGIN / 2)
        if (frame.kind == BlockKind.RECT) {
            frames.add(
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
                )
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
    return fitScene(
        Scene(
            width,
            ceil(bottomTop + headHeight + MARGIN),
            items,
            diagram.title
                ?: "Sequence diagram with $count ${if (count == 1) "participant" else "participants"}",
        ),
        measure,
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

private fun sequenceBadgeWidth(value: Double, measure: TextMeasure): Double =
    max(18.0, measure.width(jsNumberString(value), 10.0, 700) + 10)

private fun badge(value: Double, x: Double, y: Double, measure: TextMeasure): List<SceneItem> {
    val text = jsNumberString(value)
    val w = sequenceBadgeWidth(value, measure)
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
