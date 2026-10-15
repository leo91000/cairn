package build.cairn.app.diagnostics

import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.view.FrameMetrics
import android.view.Window
import build.cairn.app.BuildConfig
import build.cairn.app.data.wireJson
import java.util.UUID
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString

/** Only fixed categories enter diagnostics. Never pass a URL, ID, payload or exception here. */
enum class PerfScreen {
    STARTUP,
    FIL,
    CHAT,
    MISSIONS,
    RUN,
    ATELIER,
    SETTINGS,
    OTHER;

    companion object {
        fun fromRoute(route: String) =
            when (route) {
                "fil" -> FIL
                "chat/{id}",
                "new-chat?agent={agent}&project={project}" -> CHAT
                "missions",
                "runs" -> MISSIONS
                "run/{id}" -> RUN
                "atelier",
                "agents",
                "projects",
                "skills",
                "nodes",
                "connections",
                "mcps" -> ATELIER
                "settings" -> SETTINGS
                else -> OTHER
            }
    }
}

enum class PerfOperation {
    API_READ,
    API_WRITE,
    JSON_DECODE,
    CACHE_READ,
    CACHE_SAVE,
    HISTORY_MERGE,
    TIMELINE,
    MARKDOWN,
    LIVE_REDUCE,
    LIVE_CATCHUP,
}

enum class PerfConnection {
    NONE,
    OFFLINE,
    DIRECT,
    RELAY,
}

@Serializable
data class TimingSummary(
    val operation: PerfOperation,
    val screen: PerfScreen,
    val count: Long,
    val startedOnMainThread: Long,
    val meanMs: Double,
    val maxMs: Double,
    val p95UpperBoundMs: Double,
)

@Serializable
data class FrameSummary(
    val screen: PerfScreen,
    val swiping: Boolean,
    val count: Long,
    val missedDeadline: Long,
    val frozen: Long,
    val droppedReports: Long,
    val maxMs: Double,
    val maxUiDelayMs: Double,
    val maxLayoutMs: Double,
    val maxDrawMs: Double,
    val maxGpuMs: Double,
)

@Serializable
data class SlowSample(
    val elapsedMs: Long,
    val screen: PerfScreen,
    val kind: String,
    val durationMs: Double,
    val startedOnMainThread: Boolean,
    val swiping: Boolean,
)

@Serializable
data class PerformanceSnapshot(
    val windowId: String,
    val sequence: Long,
    val elapsedMs: Long,
    val operations: List<TimingSummary>,
    val frames: List<FrameSummary>,
    val recentSlowSamples: List<SlowSample>,
)

/** Fixed histograms and a 40-item ring; no per-frame log, disk write or network upload. */
class PerformanceStore(private val clock: () -> Long = System::nanoTime) {
    private val bounds =
        doubleArrayOf(
            1.0,
            2.0,
            4.0,
            8.0,
            16.0,
            24.0,
            32.0,
            50.0,
            100.0,
            250.0,
            500.0,
            700.0,
            1000.0,
            2000.0,
            Double.MAX_VALUE,
        )

    private class Timing {
        var count = 0L
        var main = 0L
        var sum = 0.0
        var max = 0.0
        val histogram = LongArray(15)
    }

    private class Frames {
        var count = 0L
        var missed = 0L
        var frozen = 0L
        var dropped = 0L
        var max = 0.0
        var delay = 0.0
        var layout = 0.0
        var draw = 0.0
        var gpu = 0.0
    }

    private var started = clock()
    private var windowId = UUID.randomUUID().toString()
    private var sequence = 0L
    private val operations = linkedMapOf<Pair<PerfOperation, PerfScreen>, Timing>()
    private val frames = linkedMapOf<Pair<PerfScreen, Boolean>, Frames>()
    private val slow = ArrayDeque<SlowSample>()

    @Synchronized
    fun timing(operation: PerfOperation, screen: PerfScreen, durationNs: Long, main: Boolean) {
        if (durationNs < 0) return
        sequence++
        val ms = durationNs / 1e6
        val stat = operations.getOrPut(operation to screen) { Timing() }
        stat.count++
        if (main) stat.main++
        stat.sum += ms
        stat.max = maxOf(stat.max, ms)
        stat.histogram[bounds.indexOfFirst { ms <= it }]++
        if (ms >= 50) addSlow(screen, operation.name, ms, main, false)
    }

    @Synchronized
    fun frame(
        screen: PerfScreen,
        swiping: Boolean,
        totalNs: Long,
        deadlineNs: Long,
        uiDelayNs: Long = 0,
        layoutNs: Long = 0,
        drawNs: Long = 0,
        gpuNs: Long = 0,
        dropped: Int = 0,
    ) {
        if (totalNs < 0 || deadlineNs <= 0) return
        sequence++
        val stat = frames.getOrPut(screen to swiping) { Frames() }
        val ms = totalNs / 1e6
        stat.count++
        if (totalNs >= deadlineNs) stat.missed++
        if (totalNs >= 700_000_000) stat.frozen++
        stat.dropped += dropped.coerceAtLeast(0)
        stat.max = maxOf(stat.max, ms)
        stat.delay = maxOf(stat.delay, uiDelayNs / 1e6)
        stat.layout = maxOf(stat.layout, layoutNs / 1e6)
        stat.draw = maxOf(stat.draw, drawNs / 1e6)
        stat.gpu = maxOf(stat.gpu, gpuNs / 1e6)
        if (ms >= 50) addSlow(screen, "FRAME", ms, true, swiping)
    }

    private fun addSlow(
        screen: PerfScreen,
        kind: String,
        ms: Double,
        main: Boolean,
        swiping: Boolean,
    ) {
        if (slow.size == 40) slow.removeFirst()
        slow.addLast(SlowSample((clock() - started) / 1_000_000, screen, kind, ms, main, swiping))
    }

    @Synchronized
    fun snapshot(): PerformanceSnapshot =
        PerformanceSnapshot(
            windowId,
            sequence,
            (clock() - started) / 1_000_000,
            operations.map { (key, stat) ->
                val target = kotlin.math.ceil(stat.count * .95).toLong()
                var count = 0L
                val bucket =
                    stat.histogram.indexOfFirst {
                        count += it
                        count >= target
                    }
                TimingSummary(
                    key.first,
                    key.second,
                    stat.count,
                    stat.main,
                    stat.sum / stat.count,
                    stat.max,
                    minOf(bounds[bucket], stat.max),
                )
            },
            frames.map { (key, stat) ->
                FrameSummary(
                    key.first,
                    key.second,
                    stat.count,
                    stat.missed,
                    stat.frozen,
                    stat.dropped,
                    stat.max,
                    stat.delay,
                    stat.layout,
                    stat.draw,
                    stat.gpu,
                )
            },
            slow.toList(),
        )

    @Synchronized
    fun clear() {
        operations.clear()
        frames.clear()
        slow.clear()
        started = clock()
        windowId = UUID.randomUUID().toString()
        sequence = 0L
    }
}

object Performance {
    val store = PerformanceStore()
    @Volatile var screen = PerfScreen.STARTUP
    @Volatile var swiping = false
    @Volatile var mainThread: Thread? = null
    @Volatile var connection = PerfConnection.NONE

    data class Sample(
        val operation: PerfOperation,
        val screen: PerfScreen,
        val started: Long,
        val main: Boolean,
    )

    fun start(operation: PerfOperation) =
        Sample(operation, screen, System.nanoTime(), Thread.currentThread() === mainThread)

    fun finish(sample: Sample) =
        store.timing(
            sample.operation,
            sample.screen,
            System.nanoTime() - sample.started,
            sample.main,
        )

    inline fun <T> measure(operation: PerfOperation, block: () -> T): T {
        val sample = start(operation)
        try {
            return block()
        } finally {
            finish(sample)
        }
    }

    @Serializable
    private data class Report(
        val schema: Int = 1,
        val appVersion: String = BuildConfig.VERSION_NAME,
        val buildType: String = BuildConfig.BUILD_TYPE,
        val manufacturer: String = Build.MANUFACTURER,
        val model: String = Build.MODEL,
        val androidApi: Int = Build.VERSION.SDK_INT,
        val connection: PerfConnection = Performance.connection,
        val heapUsedBytes: Long =
            Runtime.getRuntime().totalMemory() - Runtime.getRuntime().freeMemory(),
        val heapLimitBytes: Long = Runtime.getRuntime().maxMemory(),
        val measurements: PerformanceSnapshot,
    )

    fun report(snapshot: PerformanceSnapshot = store.snapshot()) =
        wireJson.encodeToString(Report(measurements = snapshot))
}

/** The listener runs on its own thread; background time never creates artificial frozen frames. */
class FrameMonitor(private val window: Window) {
    private var thread: HandlerThread? = null
    @Volatile private var active = false
    private val listener = Window.OnFrameMetricsAvailableListener { _, metrics, dropped ->
        if (active && metrics.getMetric(FrameMetrics.FIRST_DRAW_FRAME) == 0L) {
            val deadline =
                if (Build.VERSION.SDK_INT >= 31) metrics.getMetric(FrameMetrics.DEADLINE) else -1
            val refreshRate = window.decorView.display?.refreshRate?.takeIf { it > 0 } ?: 60f
            Performance.store.frame(
                Performance.screen,
                Performance.swiping,
                metrics.getMetric(FrameMetrics.TOTAL_DURATION),
                if (deadline > 0) deadline else (1e9 / refreshRate).toLong(),
                metrics.getMetric(FrameMetrics.UNKNOWN_DELAY_DURATION),
                metrics.getMetric(FrameMetrics.LAYOUT_MEASURE_DURATION),
                metrics.getMetric(FrameMetrics.DRAW_DURATION),
                if (Build.VERSION.SDK_INT >= 31) metrics.getMetric(FrameMetrics.GPU_DURATION)
                else 0,
                dropped,
            )
        }
    }

    fun start() {
        if (thread != null) return
        Performance.mainThread = Thread.currentThread()
        val worker = HandlerThread("cairn-frame-metrics").also { it.start() }
        thread = worker
        active = true
        window.addOnFrameMetricsAvailableListener(listener, Handler(worker.looper))
    }

    fun stop() {
        active = false
        if (thread == null) return
        window.removeOnFrameMetricsAvailableListener(listener)
        thread?.quitSafely()
        thread = null
        Performance.swiping = false
    }
}
