package build.cairn.app.ui

import android.os.Build
import android.os.Debug
import android.view.View
import android.view.ViewGroup
import android.view.inspector.WindowInspector
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.compose.ui.unit.dp
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import build.cairn.app.data.*
import build.cairn.app.diagnostics.*
import io.noties.markwon.Markwon
import io.noties.markwon.ext.strikethrough.StrikethroughPlugin
import io.noties.markwon.ext.tables.TablePlugin
import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Synthetic, debug-build emulator diagnostics; deliberately no frame-rate pass/fail threshold. */
@RunWith(AndroidJUnit4::class)
class ChatRenderingBenchmarkDeviceTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Before
    fun requireExplicitBenchmark() {
        assumeTrue(
            "Run with -e cairnRenderingBenchmark true; this diagnostic is intentionally opt-in.",
            InstrumentationRegistry.getArguments().getString("cairnRenderingBenchmark") == "true",
        )
    }

    private fun source(length: Int, shape: String = "prose"): String {
        val fragment =
            when (shape) {
                "code" ->
                    "## Implémentation\n\n```typescript\nexport function total(values: number[]): number {\n  return values.reduce((sum, value) => sum + value, 0)\n}\n```\n\nLe calcul est **terminé**.\n\n"
                "paragraph" ->
                    "Une très longue réponse avec du **texte en gras** et une phrase supplémentaire. "
                else ->
                    "## Étape\n\nUne **réponse détaillée** avec du texte, une liste et un lien.\n\n- Premier point\n- Deuxième point\n\n"
            }
        return fragment.repeat(length / fragment.length + 1).take(length)
    }

    private fun stats(samples: List<Double>) = buildJsonObject {
        val sorted = samples.sorted()
        put("samples", sorted.size)
        put("p50Ms", sorted[kotlin.math.ceil(sorted.size * .5).toInt() - 1])
        put("p95Ms", sorted[kotlin.math.ceil(sorted.size * .95).toInt() - 1])
        put("maxMs", sorted.last())
    }

    private fun save(name: String, measurements: JsonArray) {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val directory = File(context.filesDir, "chat-rendering-benchmark").also { it.mkdirs() }
        File(directory, name)
            .writeText(
                buildJsonObject {
                    put("api", Build.VERSION.SDK_INT)
                    put("model", Build.MODEL)
                    put("buildType", "debug")
                    put("conditions", "Synthetic fixtures; no network, storage or real account")
                    put("measurements", measurements)
                }
                    .toString()
            )
    }

    @Test
    fun timelinePreparation() {
        val results = buildJsonArray {
            for (count in listOf(40, 400, 2000)) {
                for (shape in listOf("users", "mixed", "large_answer")) {
                    val prefix =
                        (1 until count).map { index ->
                            val user = shape == "users" || index % 2 == 1
                            val text = if (user) "Question $index" else source(300)
                            RunEvent(
                                index.toLong(),
                                index.toLong(),
                                if (user) "chat.user" else "item.completed",
                                text,
                                if (user) mapOf("text" to JsonPrimitive(text))
                                else
                                    mapOf(
                                        "item" to
                                            buildJsonObject {
                                                put("id", "answer-$index")
                                                put("type", "agent_message")
                                                put("text", text)
                                            }
                                    ),
                            )
                        }
                    var text = source(if (shape == "large_answer") 100_000 else 2_000)
                    val keys = TimelineKeys()
                    val times = mutableListOf<Double>()
                    val cpu = mutableListOf<Double>()
                    val recovery = mutableListOf<Double>()
                    val identities = mutableSetOf<String>()
                    repeat(40) { sample ->
                        text += " suite $sample"
                        val last =
                            RunEvent(
                                (count + sample).toLong(),
                                count.toLong(),
                                "item.updated",
                                text,
                                mapOf(
                                    "item" to
                                        buildJsonObject {
                                            put("id", "current-answer")
                                            put("type", "agent_message")
                                            put("text", text)
                                        }
                                ),
                                displayId = count.toLong(),
                            )
                        val events = prefix + last
                        val start = System.nanoTime()
                        val cpuStart = Debug.threadCpuTimeNanos()
                        val timeline =
                            keys.stabilize("measurement", timelineEntries(events, chat = true))
                        val elapsedCpu = (Debug.threadCpuTimeNanos() - cpuStart) / 1e6
                        val elapsed = (System.nanoTime() - start) / 1e6
                        val recoveryStart = System.nanoTime()
                        val recovered =
                            recoveredChatConnections(events, events.map { it.id }.toSet())
                        val recoveryMs = (System.nanoTime() - recoveryStart) / 1e6
                        assertEquals(count, timeline.size)
                        assertTrue(recovered.isEmpty())
                        if (sample >= 10) {
                            times.add(elapsed)
                            cpu.add(elapsedCpu)
                            recovery.add(recoveryMs)
                            identities.add(timeline.last().key)
                        }
                    }
                    assertEquals(1, identities.size)
                    add(
                        buildJsonObject {
                            put("events", count)
                            put("shape", shape)
                            put("timelineWithStableKeys", stats(times))
                            put("timelineThreadCpu", stats(cpu))
                            put("reconnectionScan", stats(recovery))
                            put("messageKeys", identities.size)
                        }
                    )
                }
            }
        }
        save("timeline.json", results)
    }

    @Test
    fun nativePipeline() {
        val context = compose.activity
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val results = buildJsonArray {
            for (shape in listOf("prose", "code", "paragraph")) {
                for (length in listOf(2_000, 20_000, 100_000)) {
                    for (mode in listOf("full_text_reference", "current_blocks")) {
                        val renderer =
                            Markwon.builder(context)
                                .usePlugin(TablePlugin.create(context))
                                .usePlugin(StrikethroughPlugin.create())
                                .build()
                        val blocks = MarkdownBlocks(renderer)
                        val views = mutableListOf<MarkdownTextView>()
                        val renderTimes = mutableListOf<Double>()
                        val renderCpu = mutableListOf<Double>()
                        val layoutTimes = mutableListOf<Double>()
                        val layoutCpu = mutableListOf<Double>()
                        var text = source(length, shape)
                        var retained = 0
                        var blockCount = 0
                        var previous = emptyList<MarkdownBlock>()
                        repeat(25) { sample ->
                            text += " suite $sample"
                            val start = System.nanoTime()
                            val cpuStart = Debug.threadCpuTimeNanos()
                            val rendered =
                                if (mode == "current_blocks") blocks.render(text) else emptyList()
                            val full =
                                if (mode == "full_text_reference") renderer.toMarkdown(text)
                                else null
                            val cpuMs = (Debug.threadCpuTimeNanos() - cpuStart) / 1e6
                            val renderMs = (System.nanoTime() - start) / 1e6
                            retained = rendered.zip(previous).count { (a, b) -> a === b }
                            blockCount = rendered.size
                            previous = rendered
                            var layoutMs = 0.0
                            var mainCpuMs = 0.0
                            instrumentation.runOnMainSync {
                                val layoutStart = System.nanoTime()
                                val mainCpuStart = Debug.threadCpuTimeNanos()
                                val width = (348 * context.resources.displayMetrics.density).toInt()
                                if (mode == "current_blocks") {
                                    while (views.size < rendered.size) views.add(
                                        MarkdownTextView(context).apply {
                                            layoutParams =
                                                ViewGroup.LayoutParams(
                                                    width,
                                                    ViewGroup.LayoutParams.WRAP_CONTENT,
                                                )
                                        }
                                    )
                                    rendered.forEachIndexed { index, block ->
                                        views[index].bind(renderer, block)
                                        views[index].measure(
                                            View.MeasureSpec.makeMeasureSpec(
                                                width,
                                                View.MeasureSpec.EXACTLY,
                                            ),
                                            View.MeasureSpec.makeMeasureSpec(
                                                0,
                                                View.MeasureSpec.UNSPECIFIED,
                                            ),
                                        )
                                        views[index].layout(
                                            0,
                                            0,
                                            width,
                                            views[index].measuredHeight,
                                        )
                                    }
                                } else {
                                    val view =
                                        TextView(context).apply {
                                            layoutParams =
                                                ViewGroup.LayoutParams(
                                                    width,
                                                    ViewGroup.LayoutParams.WRAP_CONTENT,
                                                )
                                            textSize = 16f
                                            setLineSpacing(0f, 1.2f)
                                            includeFontPadding = false
                                            setTextIsSelectable(true)
                                        }
                                    renderer.setParsedMarkdown(view, full!!)
                                    view.measure(
                                        View.MeasureSpec.makeMeasureSpec(
                                            width,
                                            View.MeasureSpec.EXACTLY,
                                        ),
                                        View.MeasureSpec.makeMeasureSpec(
                                            0,
                                            View.MeasureSpec.UNSPECIFIED,
                                        ),
                                    )
                                    view.layout(0, 0, width, view.measuredHeight)
                                }
                                mainCpuMs = (Debug.threadCpuTimeNanos() - mainCpuStart) / 1e6
                                layoutMs = (System.nanoTime() - layoutStart) / 1e6
                            }
                            if (sample >= 5) {
                                renderTimes.add(renderMs)
                                renderCpu.add(cpuMs)
                                layoutTimes.add(layoutMs)
                                layoutCpu.add(mainCpuMs)
                            }
                        }
                        if (mode == "current_blocks")
                            assertTrue(views.last().text.contains("suite 24"))
                        add(
                            buildJsonObject {
                                put("shape", shape)
                                put("characters", length)
                                put("mode", mode)
                                put("markdown", stats(renderTimes))
                                put("markdownThreadCpu", stats(renderCpu))
                                put("bindAndLayout", stats(layoutTimes))
                                put("bindAndLayoutThreadCpu", stats(layoutCpu))
                                put("blocks", blockCount)
                                put("unchangedBlocks", retained)
                            }
                        )
                    }
                }
            }
            for (count in listOf(40, 400, 2000)) {
                val events =
                    (1..count).map {
                        RunEvent(it.toLong(), it.toLong(), "chat.user", "Question $it")
                    }
                val keys = TimelineKeys()
                val times = mutableListOf<Double>()
                repeat(120) { sample ->
                    val start = System.nanoTime()
                    val result = keys.stabilize("measurement", timelineEntries(events, chat = true))
                    val duration = (System.nanoTime() - start) / 1e6
                    assertEquals(count, result.size)
                    if (sample >= 20) times.add(duration)
                }
                add(
                    buildJsonObject {
                        put("events", count)
                        put("timelineWithStableKeys", stats(times))
                    }
                )
            }
        }
        save("pipeline.json", results)
    }

    private fun textViews(): List<MarkdownTextView> {
        fun collect(view: View): List<MarkdownTextView> =
            when (view) {
                is MarkdownTextView -> listOf(view)
                is ViewGroup -> (0 until view.childCount).flatMap { collect(view.getChildAt(it)) }
                else -> emptyList()
            }
        return WindowInspector.getGlobalWindowViews().flatMap(::collect)
    }

    @Test
    fun openingScrollingAndStreaming() {
        var history by mutableIntStateOf(40)
        var text by mutableStateOf("")
        var identity by mutableIntStateOf(0)
        lateinit var rendering: MarkdownRendering
        val monitor = FrameMonitor(compose.activity.window)
        compose.runOnUiThread { monitor.start() }
        Performance.screen = PerfScreen.CHAT
        compose.setContent {
            key(identity) {
                val list = rememberLazyListState(initialFirstVisibleItemIndex = history)
                rendering = remember { MarkdownRendering() }
                var follow by remember { mutableStateOf(true) }
                val gesture = rememberHistoryFollowGesture(list, follow) { follow = it }
                FollowHistoryTail(list, follow, text, rendering, gesture)
                CairnTheme("dark") {
                    Surface(Modifier.fillMaxSize()) {
                        CompositionLocalProvider(LocalMarkdownRendering provides rendering) {
                            LazyColumn(
                                Modifier.fillMaxSize()
                                    .testTag("benchmark-history")
                                    .historyFollowGesture(gesture),
                                state = list,
                                contentPadding = PaddingValues(16.dp),
                            ) {
                                items(history, key = { "prior-$it" }) { index ->
                                    Column(Modifier.padding(vertical = 6.dp)) {
                                        if (index % 2 == 0)
                                            Text("Question $index : peux-tu vérifier cette étape ?")
                                        else Markdown(source(300))
                                    }
                                }
                                item(key = "streaming") { Markdown(text) }
                            }
                        }
                    }
                }
            }
        }
        val results = buildJsonArray {
            try {
                val scenarios =
                    listOf(
                        Triple(40, 2000, "prose"),
                        Triple(400, 2000, "prose"),
                        Triple(2000, 2000, "prose"),
                        Triple(40, 20000, "prose"),
                        Triple(40, 100000, "prose"),
                        Triple(40, 20000, "code"),
                        Triple(40, 100000, "paragraph"),
                    )
                for ((count, length, shape) in scenarios) {
                    Performance.store.clear()
                    val openingMarker = "OPEN-$count-$length-$shape-END"
                    val start = System.nanoTime()
                    compose.runOnUiThread {
                        history = count
                        text = source(length, shape) + "\n\n$openingMarker"
                        identity++
                    }
                    compose.waitUntil(30_000) {
                        textViews().any { it.text.contains(openingMarker) }
                    }
                    compose.waitForIdle()
                    val openingMs = (System.nanoTime() - start) / 1e6
                    val initialViewCount = compose.runOnIdle { textViews().size }
                    val updateTimes = mutableListOf<Double>()
                    repeat(25) { sample ->
                        val marker = "STREAM-$count-$length-$shape-$sample-END"
                        val updated = System.nanoTime()
                        compose.runOnUiThread { text += "\n\n$marker" }
                        compose.waitUntil(30_000) { textViews().any { it.text.contains(marker) } }
                        val duration = (System.nanoTime() - updated) / 1e6
                        if (sample >= 5) updateTimes.add(duration)
                    }
                    compose.waitForIdle()
                    Thread.sleep(150)
                    val stream = Performance.report()
                    Performance.store.clear()
                    repeat(6) {
                        compose.onNodeWithTag("benchmark-history").performTouchInput {
                            swipeDown(durationMillis = 250)
                        }
                        compose.waitForIdle()
                    }
                    Thread.sleep(150)
                    val scroll = Performance.report()
                    add(
                        buildJsonObject {
                            put("historyRows", count)
                            put("characters", length)
                            put("shape", shape)
                            put("openingUntilTestObservedMs", openingMs)
                            put("updateUntilTestObserved", stats(updateTimes))
                            put("initialNativeTextViews", initialViewCount)
                            put("streamingDiagnostics", wireJson.parseToJsonElement(stream))
                            put("scrollingDiagnostics", wireJson.parseToJsonElement(scroll))
                            put(
                                "note",
                                "Observation includes Compose test synchronization and polling; frame diagnostics come from native FrameMetrics",
                            )
                        }
                    )
                }
            } finally {
                compose.runOnUiThread { monitor.stop() }
            }
        }
        save("ui.json", results)
    }
}
