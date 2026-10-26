package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ActivityPresentationTest {
    @Test
    fun `event-shaped diagnostic JSON remains process output`() {
        for (text in
            listOf(
                "{\"type\":\"diagnostic\",\"message\":\"disk full\",\"code\":\"ENOSPC\"}\n",
                "{\"item\":{\"type\":\"agent_message\",\"text\":\"native diagnostic\",\"id\":\"x\"}}\n",
                "{\"item\":{\"type\":\"command_execution\",\"command\":\"native diagnostic\",\"exit_code\":1}}\n",
            )) {
            val presentation = presentActivity(RunEvent(1, 1, "diagnostic", text))

            assertEquals(ActivityKind.NOTICE, presentation.kind)
            assertEquals("Diagnostic de l’agent", presentation.title)
            assertTrue(presentation.verbatim)
            assertEquals(text, presentation.output)
            assertEquals(text, presentation.raw)
            assertNull(presentation.data)
        }
    }

    @Test
    fun `shared corpus preserves activity and distinguishes immediate results`() {
        val raw =
            java.io.File(requireNotNull(System.getProperty("cairn.activityCorpus"))).readText()
        val corpus = wireJson.parseToJsonElement(raw).jsonArray
        assertEquals(17, corpus.size)
        val original =
            corpus.take(15).mapIndexed { index, value ->
                val data = value.jsonObject
                RunEvent(index.toLong() + 1, index.toLong(), data.string("type"), "", data)
            }
        val replay = timelineEntries(original)
        assertEquals(3, replay.count { it.message })
        assertEquals(1, replay.count { it.plan != null })
        val kinds =
            replay
                .filter { !it.message && it.plan == null }
                .flatMap { it.events }
                .map { presentActivity(it).kind }
        assertEquals(
            listOf(
                ActivityKind.SEARCH,
                ActivityKind.SEARCH,
                ActivityKind.READ,
                ActivityKind.READ,
                ActivityKind.BROWSE,
                ActivityKind.COMMAND,
                ActivityKind.FILES,
                ActivityKind.COMMAND,
                ActivityKind.TOOL,
            ),
            kinds,
        )

        val immediate = listOf(true, false)
        val condition = buildJsonObject {
            put("type", "run_finished")
            put("runId", "dependency")
        }
        val wakeup = ChatWakeup("wake", "message", "Pending", 60, 0, 60_000, condition = condition)
        val scheduled =
            RunEvent(1, 1, "chat.wait", "", wireJson.encodeToJsonElement(wakeup).jsonObject)
        corpus.drop(15).forEachIndexed { index, value ->
            val data = value.jsonObject
            val ready =
                RunEvent(
                    2,
                    2,
                    data.string("type"),
                    data.string("text"),
                    data["payload"] as? JsonObject,
                )
            val waits =
                timelineEntries(listOf(scheduled, ready), chat = true).mapNotNull { it.wait }
            assertEquals("case $index", if (immediate[index]) 2 else 1, waits.size)
            assertEquals(if (immediate[index]) "replaced" else null, waits.first().ended)
            val result = waits.last()
            assertEquals(if (immediate[index]) null else "wake", result.wakeup?.id)
            assertEquals("completed", result.observation.string("state"))
            assertEquals("run_finished", result.observation.string("type"))
            assertEquals("succeeded", result.observation.string("status"))
            if (immediate[index]) assertEquals("Review finished", result.reason)
        }
    }

    @Test
    fun `the technical source is the recorded JSON or the event text`() {
        val recorded = tool("pnpm test")
        assertEquals(
            activityJson.encodeToString(JsonObject(checkNotNull(recorded.payload))),
            presentActivity(recorded).raw,
        )
        assertEquals("Plain output", presentActivity(event("output", "Plain output")).raw)
    }

    private fun event(type: String, text: String) = RunEvent(1, 1000, type, text)

    private fun tool(command: String, code: Int = 0) =
        RunEvent(
            1,
            1000,
            "item.completed",
            "",
            mapOf(
                "item" to
                    buildJsonObject {
                        put("type", "command_execution")
                        put("command", command)
                        put("exit_code", code)
                    }
            ),
        )

    @Test
    fun `background waits are labelled in the activity log`() {
        fun waiting(vararg descriptions: String) =
            RunEvent(
                1,
                1000,
                "turn.waiting",
                "",
                mapOf(
                    "type" to JsonPrimitive("turn.waiting"),
                    "tasks" to
                        buildJsonArray {
                            descriptions.forEach { addJsonObject { put("description", it) } }
                        },
                ),
            )
        val waiting = presentActivity(waiting("Build", "Watch CI"))
        assertEquals(ActivityKind.NOTICE, waiting.kind)
        assertEquals("En attente de tâches en arrière-plan", waiting.title)
        assertEquals("Build · Watch CI", waiting.subtitle)
        assertEquals("", waiting.output)
        val finished = presentActivity(waiting())
        assertEquals("Tâches en arrière-plan terminées", finished.title)
        assertEquals("", finished.subtitle)
        assertEquals("", finished.output)
    }

    @Test
    fun `session events from screenshot have human summaries and no JSON body`() {
        val done = presentActivity(event("turn.completed", """{"type":"turn.completed"}"""))
        assertEquals("Travail terminé", done.title)
        assertEquals("", done.output)
        assertEquals("Exécution réussie", presentActivity(event("status", "succeeded")).title)
        val account = presentActivity(event("status", "Using Codex account: Compte principal"))
        assertEquals("Compte Codex sélectionné", account.title)
        assertEquals("Compte principal", account.subtitle)
        val claude = presentActivity(event("status", "Using Claude Code account: Studio"))
        assertEquals("Compte Claude Code sélectionné", claude.title)
        assertEquals("Studio", claude.subtitle)
    }

    @Test
    fun `quoted file reads are identified but compound shells stay commands`() {
        assertEquals(listOf("docs/my file.md"), commandView("cat 'docs/my file.md'").paths)
        assertEquals(ActivityKind.READ, commandView("bash -lc 'sed -n 1,40p README.md'").kind)
        assertEquals(ActivityKind.COMMAND, commandView("cat README.md && rm old.txt").kind)
        assertEquals(ActivityKind.COMMAND, commandView("cat \$(pwd)/README.md").kind)
        assertEquals(ActivityKind.BROWSE, commandView("rg --files").kind)
    }

    @Test
    fun `normal search outcomes are separate from failing compound commands`() {
        assertFalse(presentActivity(tool("rg needle .", 1)).failed)
        assertEquals("Aucune correspondance", presentActivity(tool("rg needle .", 1)).state)
        assertTrue(presentActivity(tool("rg needle . && build", 1)).failed)
        assertTrue(presentActivity(tool("cargo test", 1)).failed)
    }

    @Test
    fun `MCP result error is reflected on the card`() {
        val item = buildJsonObject {
            put("type", "mcp_tool_call")
            put("tool", "github__checks")
            put("result", buildJsonObject { put("isError", true) })
        }
        val p = presentActivity(RunEvent(1, 1000, "item.completed", "", mapOf("item" to item)))
        assertTrue(p.failed)
        assertEquals(ActivityKind.TOOL, p.kind)
    }

    @Test
    fun `JSON embedded in text or a fence becomes a structured result`() {
        val parts =
            resultParts(
                "Results:\n{\"jobs\":[{\"name\":\"build\",\"conclusion\":\"success\"}]}\nDone"
            )
        assertEquals(1, parts.filterIsInstance<ResultPart.Data>().size)
        assertTrue(resultParts("```json\n[1,2]\n```").single() is ResultPart.Data)
        assertTrue(resultParts("[1](https://example.com)").single() is ResultPart.Text)
        assertTrue(resultParts("`{\"a\":1}`").single() is ResultPart.Text)
    }

    @Test
    fun `truncated JSON never presents a complete nested fragment`() {
        assertTrue(
            resultParts("{\"outer\":{\"ok\":true},\"tail\":").single() is ResultPart.Incomplete
        )
        assertTrue(resultParts("```json\n{\"outer\":").single() is ResultPart.Incomplete)
    }
}
