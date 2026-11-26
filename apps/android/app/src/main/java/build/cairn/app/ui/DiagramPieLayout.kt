package build.cairn.app.ui

import kotlin.math.PI
import kotlin.math.ceil
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sin

internal fun layoutPie(diagram: PieDiagram, measure: TextMeasure): Scene {
    val items = mutableListOf<SceneItem>()
    val titleBlock = diagram.title?.let { textBlock(it, TITLE_FONT, 600, 480.0, measure) }
    val top = MARGIN + (if (titleBlock != null) titleBlock.height + 16 else 0.0)
    val total = diagram.slices.fold(0.0) { sum, slice -> sum + slice.value }
    if (!total.isFinite()) throw DiagramError("The pie total is too large to draw.")
    val radius = 92.0
    val inner = 56.0
    val cx = MARGIN + radius
    val positiveCount = diagram.slices.count { it.value > 0 }
    val rowHeight = 24.0
    fun format(value: Double) =
        if (value > 0 && value < 0.01) jsNumberString(value)
        else jsLocaleNumber(if (value < 1e12) jsRound(value * 100) / 100 else value)
    val totalText = format(total)
    val totalInLegend = measure.width(totalText, 18.0, 700) > inner * 2 - 24
    val legendHeight = (diagram.slices.size + if (totalInLegend) 1 else 0) * rowHeight
    val cy = top + max(radius, legendHeight / 2)

    var angle = 0.0
    diagram.slices.forEachIndexed { position, slice ->
        if (slice.value == 0.0) return@forEachIndexed
        val sweep = (slice.value / total) * PI * 2
        val paint = diagramTone("series-${position % SERIES}")
        // Slices are separated by a hairline gap of equal width on both rims.
        val gap = if (positiveCount == 1) 0.0 else min(0.012, sweep / 4)
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
                    fill = diagramTone("canvas"),
                )
            )
        }

        angle += sweep
    }

    items.add(
        TextItem(
            x = cx,
            y = cy - 8,
            lines = listOf(if (totalInLegend) "Σ" else totalText),
            size = 18.0,
            weight = 700,
            anchor = TextAnchor.MIDDLE,
            fill = diagramTone("ink"),
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
            fill = diagramTone("muted"),
        )
    )

    val legendX = cx + radius + 32
    var legendY = cy - legendHeight / 2 + rowHeight / 2
    var legendWidth = 0.0
    for ((position, slice) in diagram.slices.withIndex()) {
        val share = jsRound((slice.value / total) * 1000) / 10
        val label = slice.label
        val detail =
            if (diagram.showData) "${jsNumberString(share)}% · ${format(slice.value)}"
            else "${jsNumberString(share)}%"
        items.add(
            PathItem(
                d = roundedRect(legendX, legendY - 6, 12.0, 12.0, 3.5),
                fill = diagramTone("series-${position % SERIES}"),
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
                fill = diagramTone("ink"),
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
                fill = diagramTone("muted"),
            )
        )
        legendWidth = max(legendWidth, 20 + labelWidth + 8 + measure.width(detail, 12.0, 500))
        legendY += rowHeight
    }

    if (totalInLegend) {
        val label = "Total: $totalText"
        items.add(
            TextItem(
                x = legendX + 20,
                y = legendY,
                lines = listOf(label),
                size = 12.0,
                weight = 600,
                anchor = TextAnchor.START,
                fill = diagramTone("ink"),
            )
        )
        legendWidth = max(legendWidth, 20 + measure.width(label, 12.0, 600))
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
                fill = diagramTone("ink"),
            ),
        )
    }

    return Scene(
        width,
        ceil(max(cy + radius, cy + legendHeight / 2) + MARGIN),
        items,
        diagram.title
            ?: "Pie chart with ${positiveCount} ${if (positiveCount == 1) "slice" else "slices"}",
    )
}
