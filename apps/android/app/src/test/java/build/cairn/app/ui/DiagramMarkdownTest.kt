package build.cairn.app.ui

import android.app.Application
import android.content.ClipboardManager
import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import androidx.test.core.app.ApplicationProvider
import io.noties.markwon.Markwon
import io.noties.markwon.ext.strikethrough.StrikethroughPlugin
import io.noties.markwon.ext.tables.TablePlugin
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h1600dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class DiagramMarkdownTest {
    @get:Rule val compose = createComposeRule()

    private val context = ApplicationProvider.getApplicationContext<Application>()

    private val corpus by lazy {
        Json.parseToJsonElement(
                File(requireNotNull(System.getProperty("cairn.diagramCorpus"))).readText()
            )
            .jsonObject
            .getValue("parse")
            .jsonArray
            .associate {
                it.jsonObject.getValue("name").jsonPrimitive.content to
                    it.jsonObject.getValue("source").jsonPrimitive.content
            }
    }

    private fun fence(source: String) = "```mermaid\n$source\n```"

    private fun views(): List<MarkdownTextView> {
        fun collect(view: View): List<MarkdownTextView> =
            when (view) {
                is MarkdownTextView -> listOf(view)
                is ViewGroup -> (0 until view.childCount).flatMap { collect(view.getChildAt(it)) }
                else -> emptyList()
            }
        return WindowInspector.getGlobalWindowViews().flatMap(::collect)
    }

    private fun diagrams() = compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().size

    private fun show(content: () -> String, theme: () -> String = { "light" }) {
        compose.setContent {
            CairnTheme(theme()) {
                Surface(Modifier.fillMaxSize()) {
                    Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp)) {
                        Markdown(content())
                    }
                }
            }
        }
    }

    @Test
    fun `closed fences draw a card whose source and copy actions work`() {
        val source = corpus.getValue("flowchart-basic")
        var content by mutableStateOf("Avant le schéma.\n\n${fence(source)}\n\nAprès le schéma.")
        show({ content })
        compose.waitUntil(20000) { diagrams() == 1 }
        assertTrue(compose.onAllNodesWithTag("diagram-source").fetchSemanticsNodes().isEmpty())
        compose.onNodeWithTag("diagram-drawing").assertIsDisplayed()
        compose.onNodeWithText("Logigramme").assertIsDisplayed()
        assertEquals(
            listOf("Avant le schéma.", "Après le schéma."),
            views().map { it.text.toString() },
        )

        compose.onNodeWithContentDescription("Afficher le code").performClick()
        compose.onNodeWithTag("diagram-source").assertTextEquals(source)

        compose.onNodeWithContentDescription("Copier le code").performClick()
        compose.onNodeWithContentDescription("Copié").assertExists()
        assertEquals(
            source,
            context
                .getSystemService(ClipboardManager::class.java)
                .primaryClip!!
                .getItemAt(0)
                .text
                .toString(),
        )

        // Streaming more text re-renders the message; the reader's source view stays open.
        compose.runOnIdle { content += "\n\nEncore une phrase." }
        compose.waitUntil(20000) { views().lastOrNull()?.text?.contains("Encore") == true }
        compose.onNodeWithTag("diagram-source").assertExists()
        compose.onNodeWithContentDescription("Afficher le code").performClick()
        compose.onNodeWithTag("diagram-source").assertDoesNotExist()
    }

    @Test
    fun `streaming fences stay code until their closing fence arrives`() {
        var content by mutableStateOf("Voici le flux :\n\n```mermaid\nflowchart LR\n  A --> B")
        show({ content })
        compose.waitUntil(20000) { views().any { it.text.contains("A --> B") } }
        assertEquals(0, diagrams())

        compose.runOnIdle { content += "\n  B --> C" }
        compose.waitUntil(20000) { views().any { it.text.contains("B --> C") } }
        assertEquals(0, diagrams())

        compose.runOnIdle { content += "\n```" }
        compose.waitUntil(20000) { diagrams() == 1 }
        assertTrue(views().none { it.text.contains("A --> B") })
    }

    @Test
    fun `blank quoted lines preserve explicitly closed diagrams while open fences stay code`() {
        val renderer = MarkdownBlocks(Markwon.create(context))
        val open = "> ```mermaid\n> flowchart TD\n> A-->B"
        assertTrue(renderer.render("$open\n>").none { it.diagram != null })
        val closed = "$open\n> ```"
        val diagram = renderer.render(closed).single().diagram
        assertNotNull(diagram)
        for (suffix in listOf("\n>", "\n> \n>\t", "\n>\n")) assertSame(
            diagram,
            renderer.render(closed + suffix).single().diagram,
        )
    }

    @Test
    fun `unsupported kinds keep their code block and failures explain themselves`() {
        val gantt = "gantt\n  title A plan\n  section One\n  Task :a1, 2024-01-01, 3d"
        var content by mutableStateOf(fence(gantt))
        show({ content })
        compose.waitUntil(20000) { views().any { it.text.contains("Task :a1") } }
        assertEquals(0, diagrams())

        compose.runOnIdle { content = fence("flowchart TD\n  A[Start --> B") }
        compose.waitUntil(20000) { diagrams() == 1 }
        compose
            .onNodeWithText(
                "Impossible de dessiner ce diagramme. Missing “]” after a node label.",
                substring = true,
            )
            .assertIsDisplayed()
        compose.onNodeWithTag("diagram-source").assertTextEquals("flowchart TD\n  A[Start --> B")
        compose.onNodeWithContentDescription("Agrandir").assertDoesNotExist()
    }

    @Test
    fun `fences close by signature and nested fences split like tables`() {
        val markwon =
            Markwon.builder(context)
                .usePlugin(TablePlugin.create(context))
                .usePlugin(StrikethroughPlugin.create())
                .build()
        val renderer = MarkdownBlocks(markwon)
        val open = "Intro\n\n```mermaid\npie\n  \"A\" : 1"
        assertTrue(renderer.render(open).none { it.diagram != null })
        val closed = renderer.render("$open\n```")
        assertEquals(listOf(null, DiagramKind.PIE), closed.map { it.diagram?.kind })
        assertEquals("pie\n  \"A\" : 1", closed.last().diagram!!.source)
        // Only a line of the opening fence's own character closes it.
        assertTrue(renderer.render("$open\n~~~").none { it.diagram != null })
        assertTrue(renderer.render("$open\n``").none { it.diagram != null })
        // The language is the info string's first word.
        assertEquals(
            DiagramKind.PIE,
            renderer.render("```mermaid title\npie\n  \"A\" : 1\n```").single().diagram?.kind,
        )

        val nested =
            renderer.render(
                "- Étape\n\n  ```mmd\n  sequenceDiagram\n    A->>B: go\n  ```\n\n  Suite\n- Fin"
            )
        assertEquals(listOf(null, DiagramKind.SEQUENCE, null), nested.map { it.diagram?.kind })
        assertTrue(nested.last().text.toString().contains("Suite"))
        // Other languages and indented code stay text.
        val code = renderer.render("```kotlin\nval a = 1\n```\n\n    graph TD\n    A-->B")
        assertTrue(code.none { it.diagram != null })
    }

    @Test
    fun `screenshots of every diagram kind in both themes and the viewer`() {
        val dir = System.getProperty("cairn.screenshots.dir")
        val samples =
            listOf(
                "flowchart" to corpus.getValue("flowchart-subgraphs"),
                "complex" to corpus.getValue("complex"),
                "sequence" to corpus.getValue("sequence"),
                "state" to corpus.getValue("state"),
                "pie" to corpus.getValue("pie"),
            )
        var sample by mutableStateOf(samples.first().second)
        var theme by mutableStateOf("light")
        show({ fence(sample) }, { theme })
        for (name in listOf("light", "dark")) {
            for ((kind, source) in samples) {
                compose.runOnIdle {
                    theme = name
                    sample = source
                }
                val label = layoutDiagram(parseDiagram(source)) { _, _, _ -> 1.0 }.label
                compose.waitUntil(20000) {
                    compose.onAllNodesWithContentDescription(label).fetchSemanticsNodes().size == 1
                }
                compose.waitForIdle()
                if (dir != null)
                    capture(compose.onNodeWithTag("diagram"), dir, "diagram-$kind-$name")
            }
        }

        compose.runOnIdle { sample = samples.first().second }
        compose.waitUntil(20000) {
            compose
                .onAllNodesWithContentDescription("Deploy pipeline")
                .fetchSemanticsNodes()
                .size == 1
        }
        compose.onNodeWithContentDescription("Afficher le code").performClick()
        compose.waitForIdle()
        if (dir != null) capture(compose.onNodeWithTag("diagram"), dir, "diagram-source-dark")

        compose.onNodeWithContentDescription("Agrandir").performClick()
        compose.onNodeWithTag("diagram-stage").assertIsDisplayed()
        compose.onNodeWithContentDescription("Zoom avant").performClick()
        compose.onAllNodesWithContentDescription("Ajuster à l’écran").assertCountEquals(2)
        compose.waitForIdle()
        if (dir != null) capture(compose.onNode(isDialog()), dir, "diagram-viewer-dark")
        compose.onAllNodesWithContentDescription("Ajuster à l’écran").onLast().performClick()
        compose.runOnIdle { theme = "light" }
        compose.waitForIdle()
        if (dir != null) capture(compose.onNode(isDialog()), dir, "diagram-viewer-light")
        compose.onNodeWithContentDescription("Fermer").performClick()
        compose.onNodeWithTag("diagram-stage").assertDoesNotExist()
    }

    private fun capture(node: SemanticsNodeInteraction, dir: String, name: String) {
        File(dir).mkdirs()
        node.captureToImage().asAndroidBitmap().let { bitmap ->
            File(dir, "$name.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }
}
