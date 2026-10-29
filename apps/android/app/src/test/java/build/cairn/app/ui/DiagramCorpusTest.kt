package build.cairn.app.ui

import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Mermaid parsing and layout, on the corpus the web's diagram tests check too. */
class DiagramCorpusTest {
    private val corpus =
        Json.parseToJsonElement(
                File(requireNotNull(System.getProperty("cairn.diagramCorpus"))).readText()
            )
            .jsonObject

    // The web's tests use the same measure, so both layouts must agree exactly.
    private val measure = TextMeasure { text, size, _ -> text.length * size * 0.55 }

    private fun cases(name: String) = corpus.getValue(name).jsonArray.map { it.jsonObject }

    @Test
    fun `parses the shared corpus like the web`() {
        for (case in cases("parse")) {
            val name = case.getValue("name").jsonPrimitive.content
            val source = case.getValue("source").jsonPrimitive.content
            assertEquals(
                name,
                canonical(case.getValue("diagram")),
                canonical(diagramJson(parseDiagram(source))),
            )
        }
    }

    @Test
    fun `lays out the shared corpus like the web`() {
        for (case in cases("parse")) {
            val name = case.getValue("name").jsonPrimitive.content
            val scene =
                layoutDiagram(parseDiagram(case.getValue("source").jsonPrimitive.content), measure)
            val layout = case.getValue("layout").jsonObject
            assertEquals(
                name,
                listOf("width", "height", "items").map { layout.getValue(it).jsonPrimitive.double },
                listOf(scene.width, scene.height, scene.items.size.toDouble()),
            )
        }
    }

    @Test
    fun `rejects invalid diagrams`() {
        for (case in cases("errors")) {
            val name = case.getValue("name").jsonPrimitive.content
            // Parsing or layout: an empty chart parses but has nothing to draw.
            assertThrows(name, DiagramError::class.java) {
                layoutDiagram(parseDiagram(case.getValue("source").jsonPrimitive.content), measure)
            }
        }
    }

    @Test
    fun `keeps unsupported sources as code`() {
        for (source in corpus.getValue("unsupported").jsonArray.map { it.jsonPrimitive.content }) {
            assertNull(source, diagramKind(source))
            assertThrows(source, DiagramError::class.java) { parseDiagram(source) }
        }
    }

    @Test
    fun `keeps every drawing inside its bounds without overlapping nodes`() {
        for (case in cases("parse")) {
            val name = case.getValue("name").jsonPrimitive.content
            val scene =
                layoutDiagram(parseDiagram(case.getValue("source").jsonPrimitive.content), measure)
            val boxes = mutableListOf<DoubleArray>()
            for (item in scene.items) {
                if (item is TextItem) {
                    val width =
                        item.lines.maxOfOrNull { measure.width(it, item.size, item.weight) } ?: 0.0
                    val left =
                        item.x -
                            when (item.anchor) {
                                TextAnchor.MIDDLE -> width / 2
                                TextAnchor.END -> width
                                TextAnchor.START -> 0.0
                            }
                    val height = item.lines.size * lineHeight(item.size)
                    assertTrue("$name text left: $left", left >= -1)
                    assertTrue("$name text right", left + width <= scene.width + 1)
                    assertTrue("$name text top", item.y - height / 2 >= -1)
                    assertTrue("$name text bottom", item.y + height / 2 <= scene.height + 1)
                    continue
                }
                if (item !is PathItem) continue
                val points = points(item)
                for ((x, y) in points) {
                    assertTrue("$name: $x,$y", x >= -1 && y >= -1)
                    assertTrue("$name: $x,$y", x <= scene.width + 1 && y <= scene.height + 1)
                }
                if (item.shadow)
                    boxes.add(
                        doubleArrayOf(
                            points.minOf { it.first },
                            points.maxOf { it.first },
                            points.minOf { it.second },
                            points.maxOf { it.second },
                        )
                    )
            }
            for ((index, a) in boxes.withIndex()) {
                for (b in boxes.drop(index + 1)) {
                    val separate = a[1] <= b[0] || b[1] <= a[0] || a[3] <= b[2] || b[3] <= a[2]
                    assertTrue(name, separate)
                }
            }
        }
    }

    @Test
    fun `draws every flowchart node link and label`() {
        val scene =
            layoutDiagram(
                parseDiagram("flowchart LR\n  A[One] -->|go| B{Two?}\n  B --> C((Three))"),
                measure,
            )
        assertEquals(
            listOf("go", "One", "Two?", "Three"),
            scene.items.filterIsInstance<TextItem>().flatMap { it.lines },
        )
        assertEquals("Flowchart with 3 nodes", scene.label)
    }

    @Test
    fun `keeps author colors readable and links safe`() {
        val diagram =
            parseDiagram(
                "flowchart TD\n  A --> B\n  style A fill:#14365a\n  style B fill:url(#x),color:expression(alert(1))\n  click A \"javascript:alert(1)\"\n  click B \"https://example.com\""
            )
                as GraphDiagram
        assertEquals(
            listOf(DiagramStyle(fill = "#14365a") to null, null to "https://example.com/"),
            diagram.nodes.map { it.style to it.link },
        )
        val label =
            layoutDiagram(diagram, measure).items.filterIsInstance<TextItem>().first {
                it.lines.first() == "A"
            }
        assertEquals(DiagramPaint.Literal("#ffffff"), label.fill)
    }

    @Test
    fun `keeps text readable on author fills in either theme`() {
        for (case in cases("colors")) {
            val fill = case.getValue("fill").jsonPrimitive.content
            val ink = case.getValue("ink").jsonPrimitive.content
            val source = "flowchart TD\nA[Readable label]\nstyle A fill:$fill"
            val label =
                layoutDiagram(parseDiagram(source), measure)
                    .items
                    .filterIsInstance<TextItem>()
                    .first { it.lines.first() == "Readable label" }
            assertEquals(fill, DiagramPaint.Literal(ink), label.fill)
        }
    }

    @Test
    fun `preserves both self-loop arrowheads and author dash styles`() {
        val one = layoutDiagram(parseDiagram("flowchart TD\nA --> A"), measure)
        val both =
            layoutDiagram(
                parseDiagram("flowchart TD\nA <--> A\nlinkStyle 0 stroke-dasharray:4 2"),
                measure,
            )
        assertEquals(one.items.size + 1, both.items.size)
        val loop =
            both.items.filterIsInstance<PathItem>().first { item ->
                item.d.any { it is PathCommand.Cubic } && item.stroke == DiagramPaint.Tone("edge")
            }
        assertEquals(listOf(4.0, 4.0), loop.dash)
    }

    @Test
    fun `wraps long labels between words and inside long words`() {
        assertEquals(
            listOf("alpha", "beta", "gamma"),
            wrapText("alpha beta gamma", 10.0, 400, 50.0, measure),
        )
        assertEquals(
            listOf("abcde", "fghij", "klmno", "p"),
            wrapText("abcdefghijklmnop", 10.0, 400, 30.0, measure),
        )
        assertEquals(listOf("one", "two"), wrapText("one\ntwo", 10.0, 400, 200.0, measure))
    }

    @Test
    fun `wraps an adversarial long word with bounded measurement work`() {
        val source = "a".repeat(15000)
        var calls = 0
        var characters = 0
        val lines =
            wrapText(
                source,
                14.0,
                600,
                200.0,
                TextMeasure { text, size, weight ->
                    calls++
                    characters += text.length
                    measure.width(text, size, weight)
                },
            )
        assertEquals(source, lines.joinToString(""))
        assertTrue(lines.all { measure.width(it, 14.0, 600) <= 200 })
        assertTrue("measurement calls: $calls", calls < source.length)
        assertTrue("measured characters: $characters", characters < source.length * 30)
    }

    @Test
    fun `cleans Mermaid label markup with JavaScript semantics`() {
        assertEquals("A\nB & C", cleanLabel("\"A<br/>B #amp; <b>C</b>\""))
        assertEquals("é ' ✓", cleanLabel("&#233; #39; &#x2713;"))
        assertEquals("#constructor; &__proto__;", cleanLabel("#constructor; &__proto__;"))
        assertTrue(isDiagramLanguage("mermaid"))
        assertTrue(isDiagramLanguage("mmd"))
        assertFalse(isDiagramLanguage("js"))
        assertEquals("mermaid", fenceLanguage(" mermaid title "))
        // JavaScript's \s, trim() and number parsing, not the platform's Unicode rules.
        assertEquals("a", cleanLabel(" a﻿"))
        assertEquals(2.5, jsParseFloat(" 2.5px"), 0.0)
        assertEquals(16.0, jsNumber("0x10"), 0.0)
        assertEquals(0.0, jsNumber(" "), 0.0)
        assertTrue(jsNumber("1.2.3").isNaN())
        assertEquals("1,234.5", jsLocaleNumber(1234.5))
        assertEquals("42.5", jsNumberString(42.5))
        assertEquals("3", jsNumberString(3.0))
        assertEquals(3.0, jsRound(2.5), 0.0)
        assertEquals(-2.0, jsRound(-2.5), 0.0)
    }

    private fun points(item: PathItem): List<Pair<Double, Double>> =
        item.d.flatMap {
            when (it) {
                is PathCommand.Move -> listOf(it.x to it.y)
                is PathCommand.Line -> listOf(it.x to it.y)
                is PathCommand.Cubic -> listOf(it.x1 to it.y1, it.x2 to it.y2, it.x to it.y)
                PathCommand.Close -> emptyList()
            }
        }

    /** Numbers compare by value, as JSON does not distinguish 1 from 1.0. */
    private fun canonical(element: JsonElement): JsonElement =
        when (element) {
            is JsonObject -> JsonObject(element.mapValues { canonical(it.value) })
            is JsonArray -> JsonArray(element.map(::canonical))
            is JsonPrimitive ->
                if (
                    !element.isString &&
                        element.booleanOrNull == null &&
                        element.doubleOrNull != null
                )
                    JsonPrimitive(element.double)
                else element
            JsonNull -> element
        }

    private fun styleJson(style: DiagramStyle?) = style?.let {
        buildJsonObject {
            it.fill?.let { put("fill", it) }
            it.stroke?.let { put("stroke", it) }
            it.color?.let { put("color", it) }
            it.strokeWidth?.let { put("strokeWidth", it) }
            it.dashed?.let { put("dashed", it) }
        }
    }

    private fun diagramJson(diagram: Diagram): JsonObject = buildJsonObject {
        when (diagram) {
            is GraphDiagram -> {
                put("kind", "graph")
                put("direction", diagram.direction.name)
                putJsonArray("nodes") {
                    for (node in diagram.nodes) addJsonObject {
                        put("id", node.id)
                        put("label", node.label)
                        put("shape", node.shape.id)
                        node.parent?.let { put("parent", it) }
                        styleJson(node.style)?.let { put("style", it) }
                        node.link?.let { put("link", it) }
                    }
                }
                putJsonArray("edges") {
                    for (edge in diagram.edges) addJsonObject {
                        put("from", edge.from)
                        put("to", edge.to)
                        put("label", edge.label)
                        put("line", edge.line.id)
                        put("start", edge.start.id)
                        put("end", edge.end.id)
                        put("length", edge.length)
                        styleJson(edge.style)?.let { put("style", it) }
                    }
                }
                putJsonArray("clusters") {
                    for (cluster in diagram.clusters) addJsonObject {
                        put("id", cluster.id)
                        put("label", cluster.label)
                        cluster.parent?.let { put("parent", it) }
                        styleJson(cluster.style)?.let { put("style", it) }
                    }
                }
            }
            is SequenceDiagram -> {
                put("kind", "sequence")
                putJsonArray("participants") {
                    for (participant in diagram.participants) addJsonObject {
                        put("id", participant.id)
                        put("label", participant.label)
                        put("actor", participant.actor)
                    }
                }
                putJsonArray("items") {
                    for (item in diagram.items) add(itemJson(item))
                }
            }
            is PieDiagram -> {
                put("kind", "pie")
                put("showData", diagram.showData)
                putJsonArray("slices") {
                    for (slice in diagram.slices) addJsonObject {
                        put("label", slice.label)
                        put("value", slice.value)
                    }
                }
            }
        }
        diagram.title?.let { put("title", it) }
    }

    private fun itemJson(item: SequenceItem) = buildJsonObject {
        when (item) {
            is SequenceMessage -> {
                put("type", "message")
                put("from", item.from)
                put("to", item.to)
                put("label", item.label)
                put("line", item.line.id)
                put("head", item.head.id)
                put("both", item.both)
                put("activate", item.activate)
                put("deactivate", item.deactivate)
                item.number?.let { put("number", it) }
            }
            is SequenceNote -> {
                put("type", "note")
                put("placement", item.placement.id)
                putJsonArray("participants") { item.participants.forEach { add(it) } }
                put("label", item.label)
            }
            is SequenceActivation -> {
                put("type", if (item.active) "activate" else "deactivate")
                put("participant", item.participant)
            }
            is SequenceBlock -> {
                put("type", "block")
                put("kind", item.kind.id)
                put("label", item.label)
                item.color?.let { put("color", it) }
            }
            is SequenceBranch -> {
                put("type", "branch")
                put("kind", item.kind)
                put("label", item.label)
            }
            SequenceEnd -> put("type", "end")
        }
    }
}
