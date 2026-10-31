package build.cairn.app.ui

import build.cairn.app.data.RunEvent
import build.cairn.app.data.TimelineCard
import build.cairn.contracts.TimelineChange
import build.cairn.contracts.TimelineContent
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class TimelineTest {
    @Test
    fun `run log retains a changed notice whose source is outside the loaded window`() {
        val (events, cards) = timelineCorpus().first()
        val card = cards.first { it.content is TimelineContent.C }
        val entries = timelineEntries(events.filter { it.id != card.source }, cards = listOf(card))
        assertEquals(1, entries.count { it.card?.id == card.id })
    }

    @Test
    fun `the conversation and run log present the server's cards for the shared corpus`() {
        val runs = timelineCorpus()
        fun shown(index: Int) =
            timelineEntries(runs[index].first, chat = true, cards = runs[index].second)
                .mapNotNull { entry ->
                    when {
                        entry.message -> entry.card!!.messageText()
                        entry.notice ->
                            presentNotice((entry.card!!.content as TimelineContent.C).notice).title
                        entry.events.isNotEmpty() -> "group"
                        else -> null
                    }
                }
        assertEquals(
            listOf(
                "Fix the build",
                "Looking at the build. Fixed.",
                "group",
                "And the tests?",
                "Tests pass.",
                "Deploy it",
                "L’agent a rencontré une erreur après 1 nouvelle tentative automatique",
            ),
            shown(0),
        )
        assertEquals(
            listOf(
                "(private answer)",
                "Ce tour passe en vitesse Standard",
                "Working on it",
                "Avertissement de Claude Code",
                "Exécution annulée",
            ),
            shown(1),
        )
        // Plain process output remains an activity step, not a timeline card.
        assertEquals(
            listOf("Partial", "group", "L’exécuteur a échoué", "Exécution interrompue"),
            shown(2),
        )
        val via =
            timelineEntries(runs[0].first, chat = true, cards = runs[0].second).mapNotNull {
                (it.card?.content as? TimelineContent.B)?.via
            }
        assertEquals(listOf("OpenRouter"), via)

        // The run log keeps every lifecycle card among the activity, presented by its card.
        val log = timelineEntries(runs[0].first, cards = runs[0].second)
        val rows =
            log.filter { it.events.isNotEmpty() }
                .flatMap { entry ->
                    entry.events.map { e ->
                        entry.notices[e.id]?.let(::presentNotice) ?: presentActivity(e)
                    }
                }
        val connections = rows.filter { it.title == "Connexion interrompue" }
        assertEquals(2, connections.size)
        assertTrue(connections.all { it.state == RECOVERED_STATE && !it.failed })
        assertTrue(
            rows.any { it.title == "Erreur temporaire — nouvelle tentative automatique 1/3" }
        )
        val diagnostic = rows.last()
        assertEquals("Diagnostic de l’agent", diagnostic.title)
        assertTrue(diagnostic.verbatim)
        assertTrue(diagnostic.output.startsWith("{\"type\":\"turn.completed\"}"))
        // No lifecycle event renders twice: one command is the only tool step.
        assertEquals(
            1,
            rows.count { it.kind != ActivityKind.NOTICE && it.kind != ActivityKind.OUTPUT },
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
        val groups = fixtureEntries(events, chat = true)
        assertEquals(listOf(30, 30, 5), groups.map { it.events.size })
        assertEquals(65, groups.flatMap { it.events }.size)
    }

    @Test
    fun `newer tool groups keep their membership when older pages arrive or tools append`() {
        val events = (1L..191L).map { event(it, "item.completed", "tool$it") }
        for (chat in listOf(false, true)) {
            val keys = TimelineKeys()
            val loaded = keys.stabilize("h", fixtureEntries(events.subList(100, 160), chat))
            val extended = keys.stabilize("h", fixtureEntries(events, chat))
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
        val first = keys.stabilize("h", fixtureEntries(events, chat = true))
        // The person's message is its card's row, between the two full groups.
        assertEquals(listOf(30, 0, 30), first.map { it.events.size })
        assertEquals(first, keys.stabilize("h", fixtureEntries(events, chat = true)))
    }

    @Test
    fun `folded older starts preserve full group membership and key`() {
        val older = (0L..29L).map { event(it + 1, "item.started", "tool$it") }
        val loaded = (0L..59L).map { event(it + 100, "item.completed", "tool$it") }
        val keys = TimelineKeys()
        val first = keys.stabilize("h", fixtureEntries(loaded, chat = true))
        val extended = keys.stabilize("h", fixtureEntries(older + loaded, chat = true))
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
        val first = keys.stabilize("h", fixtureEntries(listOf(notice) + tools))
        val extended = keys.stabilize("h", fixtureEntries(listOf(start, notice) + tools))
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
            val rows = fixtureEntries(events, chat)
            assertEquals(34, rows.size)
            assertTrue(rows.all { it.events.size <= 30 })
            assertEquals(events.map { it.id }, rows.flatMap { it.events }.map { it.id })
            assertEquals(rows.size, rows.map { it.key }.distinct().size)
        }
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
    fun `assistant key survives appended text, cache restoration and reused ids`() {
        val accumulator = build.cairn.app.data.LiveAccumulator()
        fun reply(position: Long, revision: Long, text: String, turn: Long = 1) =
            TimelineCard(
                attempt = 0,
                content = TimelineContent.B(itemId = "answer", state = "running", text = text),
                createdAt = position,
                id = "message:$turn:answer",
                position = position,
                revision = revision,
                runId = "",
                source = revision,
                turn = turn,
                updatedAt = revision,
            )
        accumulator.append(emptyList(), listOf(TimelineChange.A(reply(1, 1, "Hel"))), 1)
        val key = timelineEntries(emptyList(), cards = accumulator.entries).single().key
        accumulator.append(
            emptyList(),
            listOf(
                TimelineChange.B(
                    id = "message:1:answer",
                    revision = 9,
                    source = 9,
                    text = "lo",
                    updatedAt = 9,
                )
            ),
            9,
        )
        assertEquals("Hello", accumulator.entries.single().messageText())
        assertEquals(key, timelineEntries(emptyList(), cards = accumulator.entries).single().key)
        val restored = build.cairn.app.data.LiveAccumulator()
        val encoded = build.cairn.app.data.wireJson.encodeToString(accumulator.entries)
        restored.restore(emptyList(), 9, build.cairn.app.data.wireJson.decodeFromString(encoded))
        assertEquals(key, timelineEntries(emptyList(), cards = restored.entries).single().key)
        restored.append(
            listOf(event(11, "turn.started").copy(timeline = true)),
            listOf(TimelineChange.A(reply(12, 12, "Next", 2))),
            12,
        )
        val keys =
            timelineEntries(restored.events, cards = restored.entries)
                .filter { it.message }
                .map { it.key }
        assertEquals(2, keys.distinct().size)
        assertEquals(key, keys.first())
        // An appended text without the baseline this stream delivered is refused.
        assertThrows(IllegalArgumentException::class.java) {
            build.cairn.app.data
                .LiveAccumulator()
                .append(
                    emptyList(),
                    listOf(
                        TimelineChange.B(
                            id = "missing",
                            revision = 2,
                            source = 2,
                            text = "x",
                            updatedAt = 2,
                        )
                    ),
                )
        }
    }

    @Test
    fun `tool updates stay in their original group across assistant messages`() {
        val rows =
            fixtureEntries(
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
            fixtureEntries(
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
            fixtureEntries(
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
            fixtureEntries(
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
            listOf(
                "message:0:answer-1",
                "files:message:0:answer-1",
                "user:3",
                "message:0:answer-2",
                "files:message:0:answer-2",
            ),
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
        val result = deliveryTimeline(fixtureEntries(listOf(event(1, "chat.user"))), listOf(file))
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
        val first = keys.stabilize("v2:run:1", fixtureEntries(window, chat = true))
        val group = first.first().key
        // The page ends inside the same group and folds the start of a command already shown.
        val page =
            listOf(
                event(5, "chat.user"),
                event(6, "item.started", "cmd-1"),
                event(7, "item.started", "cmd-2"),
            )
        val extended = keys.stabilize("v2:run:1", fixtureEntries(page + window, chat = true))
        assertEquals(listOf("user:5", group, "message:0:answer"), extended.map { it.key })
        assertEquals(3, extended[1].events.size)
        assertEquals("Keys stay unique", extended.size, extended.map { it.key }.distinct().size)
        // A different history starts from natural keys again.
        assertEquals(
            fixtureEntries(page + window, chat = true).map { it.key },
            keys.stabilize("v2:other:1", fixtureEntries(page + window, chat = true)).map {
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
        val group = keys.stabilize("h", fixtureEntries(window, chat = true)).first().key
        val extended =
            keys.stabilize(
                "h",
                fixtureEntries(listOf(event(19, "item.started", "cmd")) + window, chat = true),
            )
        assertEquals(group, extended.first().key)
    }

    /** The server's UI events and cards for the corpus web renders too. */
    private fun timelineCorpus(): List<Pair<List<RunEvent>, List<TimelineCard>>> {
        val corpus =
            build.cairn.app.data.wireJson
                .parseToJsonElement(
                    java.io.File(System.getProperty("cairn.timelineCorpus")).readText()
                )
                .jsonObject
        return corpus.getValue("runs").jsonArray.map { run ->
            val value = run.jsonObject
            build.cairn.app.data.wireJson.decodeFromJsonElement<List<RunEvent>>(
                value.getValue("events")
            ) to
                build.cairn.app.data.wireJson.decodeFromJsonElement<List<TimelineCard>>(
                    value.getValue("entries")
                )
        }
    }
}
