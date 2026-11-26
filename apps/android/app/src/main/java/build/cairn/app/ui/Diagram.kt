// Mermaid diagrams in Markdown fences, parsed into a small model that both the Android and web
// renderers lay out natively. Keep these modules in step with apps/web/src/diagram/*.ts:
// tests/fixtures/diagrams.json pins the parse results of both implementations and
// packages/contracts/diagram-spec.json is the single source of truth for limits and layout
// constants.

package build.cairn.app.ui

import build.cairn.contracts.DiagramSpec

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
    var annotation: StateAnnotation? = null
}

internal data class StateAnnotation(val target: String, val side: NotePlacement)

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

internal class GraphCluster(val id: String, var label: String) {
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
    const val SOURCE = DiagramSpec.Limits.SOURCE
    const val NODES = DiagramSpec.Limits.NODES
    const val CLUSTERS = DiagramSpec.Limits.CLUSTERS
    const val CLUSTER_DEPTH = DiagramSpec.Limits.CLUSTER_DEPTH
    const val EDGES = DiagramSpec.Limits.EDGES
    const val PARTICIPANTS = DiagramSpec.Limits.PARTICIPANTS
    const val ITEMS = DiagramSpec.Limits.ITEMS
    const val SLICES = DiagramSpec.Limits.SLICES
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
