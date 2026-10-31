package build.cairn.app.ui

import build.cairn.app.data.RunEvent
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class TimelineTest {
    @Test
    fun `reconnection scan does not serialize message payloads while preparing chat history`() {
        var payloadScans = 0
        fun observed(values: Map<String, JsonElement>): Map<String, JsonElement> =
            object : Map<String, JsonElement> by values {
                override val entries: Set<Map.Entry<String, JsonElement>>
                    get() {
                        payloadScans++
                        return values.entries
                    }
            }
        val retry =
            RunEvent(
                id = 1,
                createdAt = 1,
                type = "diagnostic",
                text = "WebSocket connection failed: 503",
            )
        val user =
            RunEvent(
                id = 2,
                createdAt = 2,
                type = "chat.user",
                text = "Question",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        observed(mapOf("text" to JsonPrimitive("Question")))
                    ),
            )
        val reply =
            RunEvent(
                id = 3,
                createdAt = 3,
                type = "item.updated",
                text = "Answer",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        observed(
                            mapOf(
                                "item" to
                                    buildJsonObject {
                                        put("id", "answer")
                                        put("type", "agent_message")
                                        put("text", "Answer ".repeat(10_000))
                                    }
                            )
                        )
                    ),
            )
        val entries = timelineEntries(listOf(retry, user, reply), chat = true)
        assertEquals(listOf(1L, 2L, 3L), entries.flatMap { it.events }.map { it.id })
        assertTrue(entries.first().notice)
        assertTrue(entries.drop(1).all { it.message })
        assertEquals(
            "Messages must not be pretty-serialized to look for connection notices",
            0,
            payloadScans,
        )
    }

    @Test
    fun `chat counts folded visible tools despite diagnostics and gaps in global event ids`() {
        val events =
            (0L..64L).flatMap { index ->
                val id = 1000L + index * 100L
                listOf(
                    event(id, "item.started", "tool$index"),
                    event(id + 1, "diagnostic").copy(text = "heartbeat"),
                    event(id + 2, "item.updated", "tool$index"),
                    event(id + 3, "item.completed", "tool$index", status = "completed"),
                )
            }
        val groups = timelineEntries(events, chat = true)
        assertEquals(listOf(30, 30, 5), groups.map { it.events.size })
        assertEquals(65, groups.flatMap { it.events }.size)
    }

    @Test
    fun `newer tool groups keep their membership when older pages arrive or tools append`() {
        val events = (1L..191L).map { event(it, "item.completed", "tool$it") }
        for (chat in listOf(false, true)) {
            val keys = TimelineKeys()
            val loaded = keys.stabilize("h", timelineEntries(events.subList(100, 160), chat))
            val extended = keys.stabilize("h", timelineEntries(events, chat))
            val group = loaded[1]
            assertEquals(group, extended.find { it.key == group.key })
        }
    }

    @Test
    fun `full groups remain stable when tool ids repeat in later turns`() {
        val events =
            (0L..29L).map { event(it + 1, "item.completed", "tool$it") } +
                listOf(event(31, "chat.user"), event(32, "turn.started")) +
                (0L..29L).map { event(it + 33, "item.completed", "tool${it - 5}") }
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(events, chat = true))
        assertEquals(listOf(30, 1, 30), first.map { it.events.size })
        assertEquals(first, keys.stabilize("h", timelineEntries(events, chat = true)))
    }

    @Test
    fun `folded older starts preserve full group membership and key`() {
        val older = (0L..29L).map { event(it + 1, "item.started", "tool$it") }
        val loaded = (0L..59L).map { event(it + 100, "item.completed", "tool$it") }
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(loaded, chat = true))
        val extended = keys.stabilize("h", timelineEntries(older + loaded, chat = true))
        assertEquals(first.map { it.key }, extended.map { it.key })
        assertEquals(listOf(30, 30), extended.map { it.events.size })
        assertEquals(
            first.map { entry -> entry.events.map { it.item().string("id") } },
            extended.map { entry -> entry.events.map { it.item().string("id") } },
        )
    }

    @Test
    fun `an older start moving before a notice preserves newer full groups`() {
        val tools = (0L..89L).map { event(it + 3, "item.completed", "tool$it") }
        val notice = event(2, "diagnostic")
        val start = event(1, "item.started", "tool0")
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(listOf(notice) + tools))
        val extended = keys.stabilize("h", timelineEntries(listOf(start, notice) + tools))
        for (group in first.subList(1, 3)) assertEquals(
            group,
            extended.find { it.key == group.key },
        )
        assertEquals(extended.size, extended.map { it.key }.distinct().size)
    }

    @Test
    fun `a thousand tools split into groups of at most thirty after folding updates`() {
        val events = (1L..1001L).map { event(it, "item.completed", "tool$it") }
        for (chat in listOf(false, true)) {
            val rows = timelineEntries(events, chat)
            assertEquals(34, rows.size)
            assertTrue(rows.all { it.events.size <= 30 })
            assertEquals(events.map { it.id }, rows.flatMap { it.events }.map { it.id })
            assertEquals(rows.size, rows.map { it.key }.distinct().size)
        }
    }

    @Test
    fun `chat labels each reply with its attempt's provider until another attempt begins`() {
        fun route(id: Long, provider: String) =
            event(id, "status")
                .copy(
                    text = "Kimi K2 via $provider",
                    payload =
                        (mapOf("route" to buildJsonObject { put("providerName", provider) }))?.let {
                            kotlinx.serialization.json.JsonObject(it)
                        },
                )
        fun attempt(id: Long) =
            event(id, "status")
                .copy(
                    text = "Preparing workspace",
                    payload =
                        (mapOf("attempt" to JsonPrimitive(true)))?.let {
                            kotlinx.serialization.json.JsonObject(it)
                        },
                )
        val events =
            listOf(
                route(1, "OpenCode Go"),
                event(2, "status").copy(text = "Opening project"),
                event(3, "item.completed", "a", "agent_message"),
                attempt(4),
                route(5, "OpenCode Zen"),
                event(6, "item.completed", "b", "agent_message"),
                attempt(7),
                event(8, "item.completed", "c", "agent_message"),
            )
        val rows = timelineEntries(events, chat = true).filter { it.message }
        assertEquals(listOf("OpenCode Go", "OpenCode Zen", ""), rows.map { it.via })
        // Run history keeps the statuses, titled by their text.
        assertEquals(events.size, timelineEntries(events).sumOf { it.events.size })
        assertEquals("Kimi K2 via OpenCode Go", presentActivity(events[0]).title)
        assertEquals("Preparing workspace", presentActivity(events[3]).title)
        assertEquals("", presentActivity(events[3]).output)
    }

    @Test
    fun `chat removes routine notices and empty groups while run history keeps them`() {
        val events =
            listOf(
                event(1, "thread.started"),
                event(2, "turn.started"),
                event(3, "status").copy(text = "running"),
                event(4, "item.started", "tool"),
                event(5, "item.completed", "tool", status = "completed"),
                event(6, "item.completed", "answer", "agent_message"),
                event(7, "turn.completed"),
                event(8, "status").copy(text = "succeeded"),
            )
        val rows = timelineEntries(events, chat = true)
        assertEquals(listOf("activity:4", "message:6"), rows.map { it.key })
        assertEquals("completed", rows.first().events.single().item().string("status"))
        assertEquals(7, timelineEntries(events).sumOf { it.events.size })
        assertTrue(timelineEntries(events.take(3), chat = true).isEmpty())
    }

    @Test
    fun `chat keeps failures and interruptions visible between tools`() {
        val events =
            listOf(
                event(1, "item.completed", "first", status = "completed"),
                event(2, "error").copy(text = "Access denied"),
                event(3, "item.completed", "second", status = "failed"),
                event(4, "status").copy(text = "failed"),
                event(5, "status").copy(text = "cancelled"),
                event(6, "status").copy(text = "interrupted"),
            )
        val rows = timelineEntries(events, chat = true)
        // The final failed status repeats the error already shown.
        assertEquals(listOf(false, true, false, true, true), rows.map { it.notice })
        assertEquals(events.map { it.id } - 4L, rows.flatMap { it.events }.map { it.id })
        assertTrue(presentActivity(rows[2].events.single()).failed)
    }

    @Test
    fun `chat hides recovered connections but preserves unresolved and failed turns`() {
        val retry =
            event(1, "diagnostic")
                .copy(text = "WebSocket connection failed: 503 Service Unavailable")
        assertTrue(timelineEntries(listOf(retry), chat = true).single().notice)
        val successfulTool =
            event(2, "item.completed", "tool", status = "completed").let {
                it.copy(
                    payload =
                        (mapOf(
                                "item" to
                                    JsonObject(it.item()!! + ("exit_code" to JsonPrimitive(0)))
                            ))
                            ?.let { kotlinx.serialization.json.JsonObject(it) }
                )
            }
        for (continued in
            listOf(
                event(2, "turn.completed"),
                event(2, "item.completed", "answer", "agent_message"),
                successfulTool,
            )) {
            assertFalse(timelineEntries(listOf(retry, continued), chat = true).any { it.notice })
        }
        val failed =
            listOf(
                retry,
                event(2, "turn.failed"),
                event(3, "turn.started"),
                event(4, "turn.completed"),
            )
        assertEquals(2, timelineEntries(failed, chat = true).count { it.notice })
        assertEquals(4, timelineEntries(failed).single().events.size)
    }

    @Test
    fun `chat shows one notice per failed attempt and marks it when Cairn retries`() {
        val capacity = "Selected model is at capacity. Please try a different model."
        fun failure(id: Long, type: String, message: String, text: String = message) =
            RunEvent(
                id = id,
                createdAt = id,
                type = type,
                text = text,
                payload =
                    kotlinx.serialization.json.JsonObject(
                        mapOf(
                            "error" to
                                buildJsonObject {
                                    put("message", message)
                                    put("codexErrorInfo", "serverOverloaded")
                                }
                        )
                    ),
                runId = "run",
            )
        fun plain(id: Long, type: String, text: String = "", payload: JsonObject? = null) =
            RunEvent(
                id = id,
                createdAt = id,
                type = type,
                text = text,
                payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
                runId = "run",
            )
        fun attempt(start: Long) =
            listOf(
                plain(start, "turn.started"),
                failure(start + 1, "turn.failed", capacity),
                failure(start + 2, "turn.failed", "Conversation interrupted."),
            )
        fun notices(events: List<RunEvent>) =
            timelineEntries(events, chat = true).filter {
                it.notice
            }

        val failed = attempt(1) + failure(4, "status", capacity, text = "failed")
        val notice = notices(failed).single()
        assertEquals(2L, notice.events.single().id)
        assertEquals(capacity, presentActivity(notice.events.single()).output)
        assertEquals(1, notices(attempt(1) + plain(4, "error", capacity)).size)

        fun retry(id: Long, attempt: Int) =
            plain(
                id,
                "retry.scheduled",
                "Temporary agent error.",
                buildJsonObject {
                    put(
                        "retry",
                        buildJsonObject {
                            put("attempt", attempt)
                            put("limit", 3)
                        },
                    )
                },
            )
        val waiting = notices(attempt(1) + retry(4, 1)).single()
        assertEquals("Erreur temporaire — nouvelle tentative automatique 1/3", waiting.noticeTitle)
        assertTrue(waiting.retrying)
        // The next attempt's notice replaces the retried one: the chain shows a single notice.
        val retried = attempt(1) + retry(4, 1) + attempt(5)
        val final = notices(retried).single()
        assertEquals(6L, final.events.single().id)
        assertEquals(
            "L’agent a rencontré une erreur après 1 nouvelle tentative automatique",
            final.noticeTitle,
        )
        assertFalse(final.retrying)
        assertEquals(
            "L’agent a rencontré une erreur après 2 nouvelles tentatives automatiques",
            notices(retried + retry(9, 2) + attempt(10)).single().noticeTitle,
        )
        val other =
            attempt(1) +
                retry(4, 1) +
                plain(5, "turn.started") +
                failure(6, "turn.failed", "Reconnect account")
        assertEquals(2, notices(other).size)
        // A run that ends, or is stopped during the wait, ends the chain: a resume is not a retry.
        assertEquals(
            listOf("L’agent a rencontré une erreur après 1 nouvelle tentative automatique", ""),
            notices(retried + attempt(8)).map { it.noticeTitle },
        )
        val cancelledWait = attempt(1) + retry(4, 1) + plain(5, "status", "cancelled") + attempt(6)
        assertEquals(
            listOf(2L, 5L, 7L),
            notices(cancelledWait).map { it.events.single().id },
        )
        // The announced retry will not run: the notice reports the failure again.
        val stoppedRetry = notices(cancelledWait).first()
        assertEquals("", stoppedRetry.noticeTitle)
        assertFalse(stoppedRetry.retrying)
        // Like web, a turn failure is a failure notice even when it reads like a connection error.
        val websocket = "WebSocket connection failed: 503 Service Unavailable"
        val websocketFailure =
            listOf(
                plain(1, "turn.started"),
                failure(2, "turn.failed", websocket),
                failure(3, "status", websocket, text = "failed"),
            )
        val websocketNotice = notices(websocketFailure).single().events.single()
        assertEquals(2L, websocketNotice.id)
        assertFalse(connectionNotice(websocketNotice))
        // The worker's final error after the last retry repeats the failure, even when it reads
        // like a connection error.
        val exhausted =
            websocketFailure.take(2) +
                retry(3, 1) +
                plain(4, "turn.started") +
                failure(5, "turn.failed", websocket) +
                plain(6, "error", websocket)
        assertEquals(listOf(5L), notices(exhausted).map { it.events.single().id })
        // Without attempt markers (older Claude histories), a new request after a failed turn
        // gets its own notice.
        val unmarked =
            listOf(
                plain(1, "chat.user", "hi"),
                failure(2, "turn.failed", capacity),
                failure(3, "status", capacity, text = "failed"),
                plain(4, "status", "Resume requested"),
                plain(5, "chat.user", "again"),
                failure(6, "turn.failed", capacity),
                failure(7, "status", capacity, text = "failed"),
            )
        assertEquals(listOf(2L, 6L), notices(unmarked).map { it.events.single().id })
        // A retry that succeeds ends the chain: a later message failing for the same reason
        // gets its own notice.
        val answered =
            attempt(1) +
                retry(4, 1) +
                plain(5, "turn.started") +
                plain(6, "turn.completed") +
                plain(7, "chat.user", "Next") +
                attempt(8)
        assertEquals(
            listOf("Erreur temporaire — nouvelle tentative automatique 1/3", ""),
            notices(answered).map { it.noticeTitle },
        )

        for (stop in
            listOf(
                "Conversation stopped.",
                "Claude execution stopped.",
                "OpenCode execution stopped.",
            )) {
            val cancelled =
                listOf(
                    plain(1, "turn.started"),
                    failure(2, "turn.failed", stop),
                    plain(3, "status", "cancelled"),
                )
            assertEquals(listOf(3L), notices(cancelled).map { it.events.single().id })
        }
        // Only an adapter's report is an echo; a run that failed with that reason still says so.
        val stopFailed =
            listOf(
                plain(1, "turn.started"),
                failure(2, "status", "Conversation stopped.", text = "failed"),
            )
        assertEquals(listOf(2L), notices(stopFailed).map { it.events.single().id })

        val timedOut =
            listOf(
                plain(1, "turn.started"),
                failure(2, "status", "Run exceeded its time limit.", text = "failed"),
            )
        val stopped = presentActivity(notices(timedOut).single().events.single())
        assertEquals("Exécution en échec", stopped.title)
        assertEquals("Run exceeded its time limit.", stopped.output)
        // A turn's reconnect attempts share one notice; the failure that ends the turn replaces it.
        fun reconnect(id: Long, step: Int) =
            plain(
                id,
                "error",
                "Reconnecting... $step/5",
                buildJsonObject {
                    put("willRetry", true)
                    put(
                        "error",
                        buildJsonObject {
                            put("message", "Reconnecting... $step/5")
                            put("codexErrorInfo", "other")
                        },
                    )
                },
            )
        val reconnecting = listOf(plain(1, "turn.started"), reconnect(2, 1), reconnect(3, 2))
        assertEquals(listOf(3L), notices(reconnecting).map { it.events.single().id })
        val disconnected =
            reconnecting + failure(4, "turn.failed", "stream disconnected before completion")
        assertEquals(listOf(4L), notices(disconnected).map { it.events.single().id })
        // The older event stream reports its reconnect attempts as plain errors.
        val plainReconnects =
            listOf(
                plain(1, "turn.started"),
                plain(2, "error", "Reconnecting... 1/5"),
                plain(3, "error", "Reconnecting... 2/5"),
            )
        assertEquals(listOf(3L), notices(plainReconnects).map { it.events.single().id })
        assertEquals(
            listOf(4L),
            notices(plainReconnects + disconnected.last()).map { it.events.single().id },
        )
        // After an error report, the generic adapter error says the turn ended.
        val reportedThenInterrupted =
            listOf(
                plain(1, "turn.started"),
                plain(2, "error", "Validation needs attention."),
                failure(3, "turn.failed", "Conversation interrupted."),
                failure(4, "status", "Conversation interrupted.", text = "failed"),
            )
        assertEquals(
            listOf(2L, 3L),
            notices(reportedThenInterrupted).map { it.events.single().id },
        )

        // Run diagnostics keep the final status without repeating the reason already reported.
        val diagnostics = timelineEntries(failed).flatMap { it.events }
        assertEquals(capacity, presentActivity(diagnostics.first { it.id == 2L }).output)
        assertEquals("", presentActivity(diagnostics.first { it.type == "status" }).output)
        assertEquals(
            "Run exceeded its time limit.",
            presentActivity(timelineEntries(timedOut).flatMap { it.events }.last()).output,
        )

        // A run records at most 10,000 characters of its error; the truncated reason still
        // repeats it.
        val long = "Upstream error: " + "x".repeat(10_050)
        val truncated =
            listOf(
                plain(1, "turn.started"),
                failure(2, "turn.failed", long),
                failure(3, "status", long.take(10_000), text = "failed"),
            )
        assertEquals(listOf(2L), notices(truncated).map { it.events.single().id })
        // A final status with a reason not shown yet tells why the run ended.
        val deniedThenTimedOut =
            listOf(timedOut[0], plain(3, "error", "Access denied")) + timedOut[1]
        assertEquals(listOf(3L, 2L), notices(deniedThenTimedOut).map { it.events.single().id })
    }

    private fun event(
        id: Long,
        type: String,
        itemId: String = "",
        itemType: String = "command_execution",
        status: String = "in_progress",
    ) =
        RunEvent(
            id = id,
            createdAt = id * 1000,
            type = type,
            text = "",
            payload =
                (if (itemId.isEmpty()) null
                    else
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("id", itemId)
                                    put("type", itemType)
                                    put("status", status)
                                }
                        ))
                    ?.let { kotlinx.serialization.json.JsonObject(it) },
        )

    @Test
    fun `assistant key survives deltas cache restoration and isolates reused ids`() {
        val accumulator = build.cairn.app.data.LiveAccumulator()
        val first = accumulator.append(listOf(event(1, "item.started", "answer", "agent_message")))
        val key = timelineEntries(first).single().key
        val updated =
            accumulator.append(listOf(event(9, "item.updated", "answer", "agent_message")))
        assertEquals(9L, updated.single().id)
        assertEquals(key, timelineEntries(updated).single().key)
        val restored = build.cairn.app.data.LiveAccumulator()
        val encoded = build.cairn.app.data.wireJson.encodeToString(updated.single())
        restored.restore(
            listOf(build.cairn.app.data.wireJson.decodeFromString<RunEvent>(encoded)),
            9,
        )
        val resumed =
            restored.append(listOf(event(10, "item.completed", "answer", "agent_message")))
        assertEquals(key, timelineEntries(resumed).single().key)
        val nextTurn =
            restored.append(
                listOf(
                    event(11, "turn.started"),
                    event(12, "item.started", "answer", "agent_message"),
                )
            )
        val keys = timelineEntries(nextTurn).filter { it.message }.map { it.key }
        assertEquals(2, keys.distinct().size)
        assertEquals(key, keys.first())
    }

    @Test
    fun `tool updates stay in their original group across assistant messages`() {
        val rows =
            timelineEntries(
                listOf(
                    event(1, "item.started", "tool"),
                    event(2, "item.completed", "answer", "agent_message"),
                    event(3, "item.completed", "tool", status = "completed"),
                )
            )
        assertEquals(2, rows.size)
        assertFalse(rows[0].message)
        assertEquals(1, rows[0].events.size)
        assertEquals("completed", rows[0].events.single().item().string("status"))
        assertEquals("activity:1", rows[0].key)
        assertTrue(rows[1].message)
    }

    @Test
    fun `reused tool ids in later turns remain independent`() {
        val rows =
            timelineEntries(
                listOf(
                    event(1, "item.completed", "tool", status = "failed"),
                    event(2, "turn.started"),
                    event(3, "item.started", "tool"),
                )
            )
        assertEquals(3, rows.single().events.size)
        assertEquals("failed", rows.single().events.first().item().string("status"))
    }

    @Test
    fun `user messages separate action groups without losing errors`() {
        val rows =
            timelineEntries(
                listOf(
                    event(1, "turn.failed"),
                    event(2, "chat.user"),
                    event(3, "item.started", "tool"),
                )
            )
        assertEquals(listOf(false, true, false), rows.map { it.message })
        assertEquals("turn.failed", rows.first().events.single().type)
    }

    @Test
    fun `files stay with their response and keep latest version per message`() {
        val entries =
            timelineEntries(
                listOf(
                    event(1, "item.completed", "answer-1", "agent_message"),
                    event(3, "chat.user"),
                    event(5, "item.completed", "answer-2", "agent_message"),
                )
            )
        fun file(id: String, message: String, version: Int, time: Long) =
            build.cairn.app.data.Deliverable(
                id,
                "run",
                messageId = message,
                key = "report",
                version = version,
                createdAt = time,
            )
        val result =
            deliveryTimeline(
                entries,
                listOf(
                    file("old", "a", 1, 500),
                    file("first", "a", 2, 900),
                    file("second", "b", 1, 4500),
                ),
            )
        assertEquals(
            listOf("message:1", "files:message:1", "message:3", "message:5", "files:message:5"),
            result.map { it.key },
        )
        assertEquals(listOf("first", "second"), result.flatMap { it.files }.map { it.id })
    }

    @Test
    fun `publication before a new user message remains before that message`() {
        val file =
            build.cairn.app.data.Deliverable(
                id = "file",
                runId = "run",
                key = "report",
                createdAt = 500,
            )
        val result = deliveryTimeline(timelineEntries(listOf(event(1, "chat.user"))), listOf(file))
        assertEquals("file", result.first().files.single().id)
        assertTrue(result.last().message)
        assertEquals("file", deliveryTimeline(emptyList(), listOf(file)).single().files.single().id)
    }

    @Test
    fun `an older page extending the first activity group keeps its row key`() {
        val keys = TimelineKeys()
        val window =
            listOf(
                event(10, "item.completed", "cmd-2", status = "completed"),
                event(11, "item.completed", "cmd-3", status = "completed"),
                event(12, "item.completed", "answer", "agent_message"),
            )
        val first = keys.stabilize("v1:run:1", timelineEntries(window, chat = true))
        val group = first.first().key
        // The page ends inside the same group and folds the start of a command already shown.
        val page =
            listOf(
                event(5, "chat.user"),
                event(6, "item.started", "cmd-1"),
                event(7, "item.started", "cmd-2"),
            )
        val extended = keys.stabilize("v1:run:1", timelineEntries(page + window, chat = true))
        assertEquals(listOf("message:5", group, "message:12"), extended.map { it.key })
        assertEquals(3, extended[1].events.size)
        assertEquals("Keys stay unique", extended.size, extended.map { it.key }.distinct().size)
        // A different history starts from natural keys again.
        assertEquals(
            timelineEntries(page + window, chat = true).map { it.key },
            keys.stabilize("v1:other:1", timelineEntries(page + window, chat = true)).map {
                it.key
            },
        )
    }

    @Test
    fun `a folded command that was the only row of the first group keeps its key`() {
        val keys = TimelineKeys()
        val window =
            listOf(
                event(20, "item.completed", "cmd", status = "completed"),
                event(21, "item.completed", "answer", "agent_message"),
            )
        val group = keys.stabilize("h", timelineEntries(window, chat = true)).first().key
        val extended =
            keys.stabilize(
                "h",
                timelineEntries(listOf(event(19, "item.started", "cmd")) + window, chat = true),
            )
        assertEquals(group, extended.first().key)
    }
}
