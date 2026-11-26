package build.cairn.app.ui

import build.cairn.contracts.DiagramSpec

// Lays a parsed diagram out as a flat scene of paths and text. Renderers only map tones to
// theme colors, so Android and the web (apps/web/src/diagram/*) draw the same geometry.
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

internal fun tone(name: String) = DiagramPaint.Tone(name)

internal const val NODE_FONT = DiagramSpec.Layout.NODE_FONT
internal const val LABEL_FONT = DiagramSpec.Layout.LABEL_FONT
internal const val TITLE_FONT = DiagramSpec.Layout.TITLE_FONT
internal const val CLUSTER_FONT = DiagramSpec.Layout.CLUSTER_FONT
internal const val WRAP = DiagramSpec.Layout.WRAP
internal const val MARGIN = DiagramSpec.Layout.MARGIN
internal const val NODE_SEP = DiagramSpec.Layout.NODE_SEP
internal const val EDGE_SEP = DiagramSpec.Layout.EDGE_SEP
internal const val RANK_GAP = DiagramSpec.Layout.RANK_GAP
internal const val CLUSTER_PAD = DiagramSpec.Layout.CLUSTER_PAD
internal const val ARROW = DiagramSpec.Layout.ARROW
internal const val MAX_LAYOUT_NODES = DiagramSpec.Layout.MAX_LAYOUT_NODES
internal const val MAX_LAYOUT_CONSTRAINTS = DiagramSpec.Layout.MAX_LAYOUT_CONSTRAINTS
internal const val MAX_SCENE_SIDE = DiagramSpec.Layout.MAX_SCENE_SIDE
internal const val KAPPA = DiagramSpec.Layout.KAPPA
internal const val SEQ_FONT = DiagramSpec.Layout.SEQ_FONT
internal const val ACTOR_GAP = DiagramSpec.Layout.ACTOR_GAP
internal const val SERIES = DiagramSpec.Layout.SERIES

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
