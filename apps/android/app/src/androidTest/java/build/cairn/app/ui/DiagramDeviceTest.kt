package build.cairn.app.ui

import android.app.Activity
import android.app.Instrumentation
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.BitmapFactory
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Real Canvas, pointer gestures and FileProvider sharing on Android 16. */
@RunWith(AndroidJUnit4::class)
class DiagramDeviceTest {
    @get:Rule val compose = createComposeRule()
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext

    private fun show(
        density: Float? = null,
        openArtifact: (String) -> Boolean = { false },
        content: () -> String,
    ) {
        compose.setContent {
            val platformDensity = LocalDensity.current
            val testDensity =
                density?.let { Density(it, platformDensity.fontScale) } ?: platformDensity
            CompositionLocalProvider(
                LocalDensity provides testDensity,
                LocalArtifactLinks provides openArtifact,
            ) {
                CairnTheme("light") {
                    Surface(Modifier.fillMaxSize()) {
                        Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp)) {
                            Markdown(content())
                        }
                    }
                }
            }
        }
    }

    @Test
    fun streamingSourceLinksViewerGesturesAndPngSharing() {
        val source = "flowchart TD\nA[Documentation]\nclick A \"https://example.test/docs\""
        var content by mutableStateOf("```mermaid\n$source")
        val intents = CopyOnWriteArrayList<Intent>()
        val monitor =
            object : Instrumentation.ActivityMonitor() {
                override fun onStartActivity(intent: Intent): Instrumentation.ActivityResult? {
                    if (
                        intent.action == Intent.ACTION_VIEW ||
                            intent.action == Intent.ACTION_CHOOSER
                    ) {
                        intents.add(Intent(intent))
                        return Instrumentation.ActivityResult(Activity.RESULT_CANCELED, null)
                    }
                    return null
                }
            }
        instrumentation.addMonitor(monitor)
        try {
            show { content }
            compose.waitForIdle()
            compose.onNodeWithTag("diagram").assertDoesNotExist()
            compose.runOnIdle { content += "\n```" }
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().size == 1
            }
            compose.onNodeWithTag("diagram-drawing").assertIsDisplayed()
            compose.onNodeWithContentDescription("Afficher le code").performClick()
            compose.onNodeWithTag("diagram-source").assertTextEquals(source)
            compose.onNodeWithContentDescription("Copier le code").performClick()
            assertEquals(
                source,
                context
                    .getSystemService(ClipboardManager::class.java)
                    .primaryClip!!
                    .getItemAt(0)
                    .text
                    .toString(),
            )
            compose.onNodeWithContentDescription("Afficher le code").performClick()
            compose.onNodeWithContentDescription("https://example.test/docs").performTouchInput {
                click(center)
            }
            compose.waitUntil(10000) { intents.any { it.action == Intent.ACTION_VIEW } }
            assertEquals(
                "https://example.test/docs",
                intents.first { it.action == Intent.ACTION_VIEW }.dataString,
            )

            compose.onNodeWithContentDescription("Agrandir").performClick()
            val stage = compose.onNodeWithTag("diagram-stage")
            stage.assertIsDisplayed()
            stage.performTouchInput { click(center) }
            compose.waitUntil(10000) { intents.count { it.action == Intent.ACTION_VIEW } == 2 }
            fun zoom() =
                compose
                    .onAllNodesWithContentDescription("Ajuster à l’écran")
                    .onFirst()
                    .fetchSemanticsNode()
                    .config[SemanticsProperties.Text]
                    .joinToString { it.text }
            val fitted = zoom()
            val beforeTwoFingerPan = stage.captureToImage().asAndroidBitmap()
            stage.performTouchInput {
                pinch(
                    center - Offset(40f, 0f),
                    center + Offset(40f, 0f),
                    center - Offset(40f, 0f) + Offset(80f, 50f),
                    center + Offset(40f, 0f) + Offset(80f, 50f),
                )
            }
            compose.waitForIdle()
            assertEquals("Two-finger translation must keep the scale", fitted, zoom())
            val afterTwoFingerPan = stage.captureToImage().asAndroidBitmap()
            assertFalse(
                "Two-finger translation must move the drawing",
                beforeTwoFingerPan.sameAs(afterTwoFingerPan),
            )
            beforeTwoFingerPan.recycle()
            afterTwoFingerPan.recycle()
            stage.performTouchInput { doubleClick(center) }
            compose.waitForIdle()
            assertEquals(fitted, zoom())
            stage.performTouchInput {
                pinch(
                    center - Offset(40f, 0f),
                    center + Offset(40f, 0f),
                    center - Offset(100f, 0f),
                    center + Offset(100f, 0f),
                )
            }
            compose.waitForIdle()
            assertNotEquals(fitted, zoom())
            val beforePan = stage.captureToImage().asAndroidBitmap()
            stage.performTouchInput { swipe(center, center + Offset(80f, 50f)) }
            compose.waitForIdle()
            val afterPan = stage.captureToImage().asAndroidBitmap()
            assertFalse("Panning must move the actual drawing", beforePan.sameAs(afterPan))
            beforePan.recycle()
            afterPan.recycle()
            stage.performTouchInput { click(center + Offset(80f, 50f)) }
            compose.waitUntil(10000) { intents.count { it.action == Intent.ACTION_VIEW } == 3 }
            stage.performTouchInput { doubleClick(center) }
            compose.waitForIdle()
            assertEquals(fitted, zoom())

            compose.onNodeWithContentDescription("Partager l’image").performClick()
            compose.waitUntil(20000) { intents.any { it.action == Intent.ACTION_CHOOSER } }
            val chooser = intents.first { it.action == Intent.ACTION_CHOOSER }
            val send = chooser.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)!!
            assertEquals(Intent.ACTION_SEND, send.action)
            assertEquals("image/png", send.type)
            assertTrue(send.flags and Intent.FLAG_GRANT_READ_URI_PERMISSION != 0)
            val uri = send.getParcelableExtra(Intent.EXTRA_STREAM, android.net.Uri::class.java)!!
            assertEquals(uri, send.clipData!!.getItemAt(0).uri)
            context.contentResolver.openInputStream(uri).use { stream ->
                val bitmap = BitmapFactory.decodeStream(stream)
                assertNotNull("Shared URI must expose a readable PNG", bitmap)
                assertTrue(bitmap.width > 0 && bitmap.height > 0)
                bitmap.recycle()
            }
            val firstImage = context.contentResolver.openInputStream(uri)!!.use { it.readBytes() }
            compose.onNodeWithContentDescription("Fermer").performClick()
            stage.assertDoesNotExist()
            compose.runOnIdle { content = "" }
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().isEmpty()
            }
            compose.runOnIdle {
                content = "```mermaid\nflowchart TD\nA[A different private diagram]\n```"
            }
            compose.waitUntil(20000) {
                compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().size == 1
            }
            compose.onNodeWithContentDescription("Agrandir").performClick()
            compose.onNodeWithContentDescription("Partager l’image").performClick()
            compose.waitUntil(20000) { intents.count { it.action == Intent.ACTION_CHOOSER } == 2 }
            val secondChooser = intents.last { it.action == Intent.ACTION_CHOOSER }
            val secondSend =
                secondChooser.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)!!
            val secondUri =
                secondSend.getParcelableExtra(Intent.EXTRA_STREAM, android.net.Uri::class.java)!!
            assertNotEquals("Each export must have its own grantable URI", uri, secondUri)
            assertArrayEquals(
                firstImage,
                context.contentResolver.openInputStream(uri)!!.use { it.readBytes() },
            )
            val secondImage =
                context.contentResolver.openInputStream(secondUri)!!.use { it.readBytes() }
            assertFalse(
                "The second export must contain its own image",
                firstImage.contentEquals(secondImage),
            )
            context.contentResolver.query(secondUri, null, null, null, null)!!.use { cursor ->
                assertTrue(cursor.moveToFirst())
                assertEquals(
                    "organigramme.png",
                    cursor.getString(
                        cursor.getColumnIndexOrThrow(android.provider.OpenableColumns.DISPLAY_NAME)
                    ),
                )
            }
        } finally {
            instrumentation.removeMonitor(monitor)
        }
    }

    @Test
    fun artifactNodesUseTheSameInAppOpenerInCardAndViewer() {
        val link = "https://example.test/api/runs/run-1/artifacts/file-1"
        val opened = CopyOnWriteArrayList<String>()
        val source = "flowchart TD\nA[Report]\nclick A \"$link\""
        show(
            openArtifact = {
                opened.add(it)
                true
            }
        ) {
            "```mermaid\n$source\n```"
        }
        compose.waitUntil(20000) {
            compose.onAllNodesWithTag("diagram-drawing").fetchSemanticsNodes().size == 1
        }
        compose.onNodeWithContentDescription(link).performTouchInput { click(center) }
        compose.waitUntil(10000) { opened.size == 1 }
        compose.onNodeWithContentDescription("Agrandir").performClick()
        compose.onNodeWithTag("diagram-stage").performTouchInput { click(center) }
        compose.waitUntil(10000) { opened.size == 2 }
        assertEquals(listOf(link, link), opened.toList())
    }

    @Test
    fun largeTwoDimensionalSceneFitsNativePreviewAtHighDensity() {
        val source =
            "flowchart LR\n" +
                (0 until 90).joinToString("\n") { "N$it[${"W".repeat(28)}]-->N${it + 1}" } +
                "\nN50[\"${List(230) { "A descriptive step" }.joinToString("<br>")}\"]"
        val scene = renderDiagram(source, DiagramFonts.get(context))!!.scene
        assertNotNull("The large fixture must stay inside supported scene limits", scene)
        val density = Density(4f)
        val oldPixels =
            with(density) {
                (scene!!.width * 0.75).dp.roundToPx() to (scene.height * 0.75).dp.roundToPx()
            }
        assertThrows(IllegalArgumentException::class.java) {
            Constraints.fixed(oldPixels.first, oldPixels.second)
        }
        show(density = 4f) { "```mermaid\n$source\n```" }
        compose.waitUntil(20000) {
            compose.onAllNodesWithTag("diagram-drawing").fetchSemanticsNodes().size == 1
        }
        compose.onNodeWithTag("diagram-drawing").assertIsDisplayed()
        compose.onNodeWithContentDescription("Agrandir").performClick()
        compose.onNodeWithTag("diagram-stage").assertIsDisplayed()
        compose.onNodeWithContentDescription("Fermer").performClick()
    }

    @Test
    fun largeDiagramFitsBelowFifteenPercent() {
        val source =
            "flowchart LR\n" + (0 until 100).joinToString("\n") { "A$it[Step $it] --> A${it + 1}" }
        show { "```mermaid\n$source\n```" }
        compose.waitUntil(20000) {
            compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().size == 1
        }
        compose.onNodeWithContentDescription("Agrandir").performClick()
        val stage = compose.onNodeWithTag("diagram-stage")
        stage.assertIsDisplayed()
        fun zoomPercent(): Int =
            compose
                .onAllNodesWithContentDescription("Ajuster à l’écran")
                .onFirst()
                .fetchSemanticsNode()
                .config[SemanticsProperties.Text]
                .single()
                .text
                .substringBefore(' ')
                .toInt()
        compose.waitUntil(10000) { zoomPercent() in 1..14 }
        val scene = renderDiagram(source, DiagramFonts.get(context))!!.scene!!
        val bounds = stage.fetchSemanticsNode().boundsInRoot
        val density = context.resources.displayMetrics.density
        val available =
            minOf(
                bounds.width / (scene.width * density),
                bounds.height / (scene.height * density),
            ) * 100
        assertTrue(zoomPercent() <= available + 0.5)
        compose.onNodeWithContentDescription("Zoom avant").performClick()
        compose.onAllNodesWithContentDescription("Ajuster à l’écran").onLast().performClick()
        compose.waitUntil(10000) { zoomPercent() in 1..14 }
    }

    @Test
    fun allKindsAndMalformedHierarchyUseNativeCardsInBothThemes() {
        var content by mutableStateOf("")
        var theme by mutableStateOf("light")
        compose.setContent {
            CairnTheme(theme) {
                Surface(Modifier.fillMaxSize()) {
                    Column(Modifier.verticalScroll(rememberScrollState()).padding(16.dp)) {
                        Markdown(content)
                    }
                }
            }
        }
        for (mode in listOf("light", "dark")) {
            for (source in
                listOf(
                    "flowchart LR\nA-->B",
                    "flowchart TB\nsubgraph S1\nN2\nN8\nend\nsubgraph S2\nN0\nN9\nend\nN3-->N8\nN3-->N7\nN10-->N0",
                    "flowchart LR\nsubgraph S[Long descriptive group title]\nA[Go]\nend\nsubgraph T[Another group]\nB[Done]\nend\nS-->T",
                    "flowchart TD\nA{Ready?}-->|retry|A",
                    "flowchart TD\nA>\"" + List(11) { "Hello" }.joinToString("<br>") + "\"]",
                    "stateDiagram-v2\n[*]-->Ready\nnote left of Ready : Input\nnote right of Ready : Output",
                    "stateDiagram-v2\nclassDef critical fill:red\nA-->B:::critical : proceed",
                    "sequenceDiagram\nA->>B: Hello",
                    "sequenceDiagram\nparticipant A\nloop Outer\nloop Inner\nA->>A: Hi\nend\nend",
                    "sequenceDiagram\nparticipant A\nrect rgb(255,0,0)\nrect rgb(0,255,0)\nA->>A: Hi\nend\nend",
                    "sequenceDiagram;A->>B: first #59;;B->>A: response",
                    "sequenceDiagram\nparticipant A\n" + "activate A\n".repeat(20),
                    "sequenceDiagram\nautonumber 1234567890123\nA->>A: work",
                    "sequenceDiagram\nparticipant A\nparticipant B\n" +
                        "activate A\n".repeat(20) +
                        "A->>A: abcdefghijabcdefghijabcdefghij",
                    "pie\n\"A\" : 1\n\"B\" : 2",
                    "pie showData\n\"Completed\":10\n\"Failed\":0",
                )) {
                compose.runOnIdle { content = "" }
                compose.waitUntil(20000) {
                    compose.onAllNodesWithTag("diagram").fetchSemanticsNodes().isEmpty()
                }
                compose.runOnIdle {
                    theme = mode
                    content = "```mermaid\n$source\n```"
                }
                val label =
                    layoutDiagram(
                            parseDiagram(source),
                            TextMeasure { text, size, _ -> text.length * size * 0.55 },
                        )
                        .label
                compose.waitUntil(20000) {
                    compose.onAllNodesWithContentDescription(label).fetchSemanticsNodes().size == 1
                }
                compose.onNodeWithTag("diagram-drawing").assertIsDisplayed()
                compose.onNodeWithContentDescription("Agrandir").performClick()
                compose.onNodeWithTag("diagram-stage").assertIsDisplayed()
                compose.onNodeWithContentDescription("Fermer").performClick()
            }
        }
        for (source in
            listOf(
                "flowchart TD\nsubgraph S\nsubgraph S\nA\nend\nend",
                "stateDiagram-v2\nstate Active {\nIdle-->Running\n}\nActive --> Active : retry",
                "flowchart TD\nsubgraph Active\nIdle-->Running\nend\nActive-->|retry|Active",
                "flowchart TD\nsubgraph S\nA\nend\nS-->|start|A",
                "stateDiagram-v2\nstate Active {\n[*]-->A\nA-->[*]\n--\n[*]-->B\nB-->[*]\n}",
                "sequenceDiagram\nparticipant A\ncreate participant B\nA->>B: work",
                "sequenceDiagram\nparticipant A\ncreate actor B\nA->>B: work",
                "sequenceDiagram\nparticipant A\ndestroy A\nA->>B: work",
                "sequenceDiagram\nbox Internal APIs\nparticipant A\nparticipant B\nend\nA->>B: work",
            )) {
            compose.runOnIdle { content = "```mermaid\n$source\n```" }
            compose.waitUntil(20000) {
                compose
                    .onAllNodes(hasTestTag("diagram-source") and hasText(source))
                    .fetchSemanticsNodes()
                    .size == 1
            }
            compose.onNodeWithTag("diagram-source").assertTextEquals(source)
            compose
                .onNodeWithText("Impossible de dessiner ce diagramme.", substring = true)
                .assertIsDisplayed()
            compose.onNodeWithContentDescription("Agrandir").assertDoesNotExist()
        }
    }
}
