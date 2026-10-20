package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Main-thread work done each time an older page is prepended while scrolling up a long
 * conversation, on the JVM. Deliberately not a device frame-rate assertion.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class HistoryPagingCostTest {
    private val output = "Synthetic command output line.\n".repeat(50)
    private val reply =
        "Une **réponse détaillée** avec du texte et une liste.\n\n- Point\n".repeat(16)

    /** One turn: a message, eight tool calls, a reply and its status, as the server pages it. */
    private fun turn(first: Long): List<RunEvent> {
        var id = first
        fun next() = id++
        val events = mutableListOf<RunEvent>()
        events +=
            RunEvent(
                next(),
                first,
                "chat.user",
                "Question $first",
                mapOf("text" to JsonPrimitive("Question $first")),
            )
        events += RunEvent(next(), first, "turn.started", "")
        repeat(8) { tool ->
            for (status in listOf("in_progress", "completed")) {
                val event = next()
                events +=
                    RunEvent(
                        event,
                        event,
                        if (status == "completed") "item.completed" else "item.started",
                        "",
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("id", "tool-$first-$tool")
                                    put("type", "command_execution")
                                    put("command", "pnpm test --filter $tool")
                                    put("status", status)
                                    if (status == "completed") {
                                        put("aggregated_output", output)
                                        put("exit_code", 0)
                                    }
                                }
                        ),
                    )
            }
        }
        val answer = next()
        events +=
            RunEvent(
                answer,
                answer,
                "item.completed",
                reply,
                mapOf(
                    "item" to
                        buildJsonObject {
                            put("id", "answer-$first")
                            put("type", "agent_message")
                            put("text", reply)
                        }
                ),
            )
        events += RunEvent(next(), answer, "status", "succeeded")
        return events
    }

    @Test
    fun `measure each prepended page of a long conversation`() {
        // Earlier sweeps warm up the JIT; only the last one is reported.
        repeat(2) { sweep() }
        sweep().forEach { println("PAGING_PROFILE $it") }
    }

    private fun sweep(): Collection<String> {
        val pageCount = 40
        val all = (0 until (pageCount + 1) * 5).flatMap { turn(it * 20L + 1) }
        val pages = all.chunked(100)
        val recent = pages.last()
        var live =
            LiveSnapshot(
                events = recent,
                history = "v1:fixture:1",
                cursor = recent.last().id,
                oldest = recent.first().id,
                hasOlder = true,
                catchingUp = false,
            )
        val older = OlderPages()
        val keys = TimelineKeys()
        var displayed = older.display(live)
        val rows = mutableMapOf<Int, String>()

        fun ms(start: Long) = (System.nanoTime() - start) / 1e6
        fun derive(events: List<RunEvent>): List<TimelineEntry> {
            val timeline =
                deliveryTimeline(
                    keys.stabilize(live.history, timelineEntries(events, chat = true)),
                    emptyList(),
                )
            subagentTeams(timeline)
            currentSubagents(timeline)
            return timeline
        }
        derive(displayed.events)

        for (loaded in 1..pageCount) {
            val events = pages[pages.size - 1 - loaded]
            val page = HistoryPage(events, "v1:fixture:1", events.first().id, loaded < pageCount)

            // Applying the page and deriving the rows happen on the main thread before the
            // next frame can be drawn.
            var start = System.nanoTime()
            displayed = checkNotNull(older.apply(page, live, displayed))
            val applyMs = ms(start)

            start = System.nanoTime()
            val timeline = derive(displayed.events)
            val deriveMs = ms(start)
            start = System.nanoTime()
            val entries = timelineEntries(displayed.events, chat = true)
            val entriesMs = ms(start)
            start = System.nanoTime()
            subagentTimeline(displayed.events)
            val subagentMs = ms(start)
            start = System.nanoTime()
            val stable = TimelineKeys().stabilize(live.history, entries)
            val stabilizeMs = ms(start)
            start = System.nanoTime()
            deliveryTimeline(stable, emptyList())
            val deliveryMs = ms(start)

            // A later live emission re-merges every loaded page.
            live = live.copy(cursor = live.cursor + 1)
            start = System.nanoTime()
            displayed = older.display(live)
            val liveMs = ms(start)

            start = System.nanoTime()
            recentHistory(
                CachedHistory(
                    displayed.cursor,
                    "v1:fixture:1",
                    LiveState(),
                    displayed.events,
                    oldest = displayed.oldest,
                    hasOlder = displayed.hasOlder,
                )
            )
            val cacheMs = ms(start)

            assertEquals((loaded + 1) * 100, displayed.events.size)
            if (loaded in setOf(1, 5, 10, 20, 30, 40))
                rows[loaded] =
                    buildJsonObject {
                        put("pages", loaded)
                        put("events", displayed.events.size)
                        put("rows", timeline.size)
                        put("apply_ms", applyMs)
                        put("derive_ms", deriveMs)
                        put("entries_ms", entriesMs)
                        put("subagents_ms", subagentMs)
                        put("stabilize_ms", stabilizeMs)
                        put("delivery_ms", deliveryMs)
                        put("live_emission_ms", liveMs)
                        put("cache_window_off_main_ms", cacheMs)
                    }
                        .toString()
        }
        return rows.values
    }
}
