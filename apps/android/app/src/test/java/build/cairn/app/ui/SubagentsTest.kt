package build.cairn.app.ui

import build.cairn.app.data.Chat
import build.cairn.app.data.RunEvent
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Mirrors the web subagent cases so both clients fold the same events the same way. */
class SubagentsTest {
    private var serial = 0L

    private fun event(type: String, payload: JsonObject, runId: String = "run"): RunEvent {
        val id = ++serial
        return RunEvent(id, id * 1000, type, "", payload, runId = runId)
    }

    private fun item(type: String, runId: String = "run", body: JsonObjectBuilder.() -> Unit) =
        event(type, buildJsonObject { put("item", buildJsonObject(body)) }, runId)

    private fun user(text: String) =
        event(
            "chat.user",
            buildJsonObject {
                put("text", text)
                put("messageId", text)
            },
        )

    private fun subagent(
        type: String,
        id: String,
        runId: String = "run",
        body: JsonObjectBuilder.() -> Unit = {},
    ) =
        item(type, runId) {
            put("id", id)
            put("type", "subagent")
            put("name", id)
            put("agent_type", "Explore")
            put("prompt", "Prompt for $id")
            put("status", "in_progress")
            body()
        }

    private fun command(
        id: String,
        command: String,
        parent: String? = null,
        running: Boolean = false,
    ) =
        item(if (running) "item.started" else "item.completed") {
            put("id", id)
            put("type", "command_execution")
            put("command", command)
            if (parent != null) put("parent_id", parent)
            if (running) put("status", "in_progress") else put("exit_code", 0)
        }

    private fun message(id: String, text: String, parent: String? = null) =
        item("item.completed") {
            put("id", id)
            put("type", if (parent == null) "agent_message" else "subagent_message")
            put("text", text)
            if (parent != null) put("parent_id", parent)
        }

    private fun turn(type: String) = event(type, buildJsonObject { put("type", type) })

    private fun teams(events: List<RunEvent>) =
        timelineEntries(events, chat = true).filter { it.agents.isNotEmpty() }.map { it.agents }

    private fun Subagent.commands() = steps.map {
        (it as SubagentStep.Action).activity.command to it.activity.state
    }

    @Test
    fun `groups subagents started together and keeps their steps and messages out of the reply`() {
        val timeline =
            timelineEntries(
                listOf(
                    user("Audit the webhooks"),
                    message("intro", "Starting two subagents."),
                    subagent("item.started", "Webhooks"),
                    subagent("item.started", "Tests"),
                    command("grep", "rg -n webhook", parent = "Webhooks"),
                    message("note", "Found three handlers.", parent = "Webhooks"),
                    subagent("item.updated", "Webhooks") { put("activity", "Reading webhook.rs") },
                    subagent("item.completed", "Tests") {
                        put("status", "completed")
                        put("result", "All billing tests pass.")
                        put("duration_ms", 4200)
                        put("tokens", 1510)
                    },
                    message("reply", "Waiting for the webhook audit."),
                ),
                chat = true,
            )

        assertEquals(
            listOf("message", "message", "team", "message"),
            timeline.map {
                if (it.agents.isNotEmpty()) "team" else if (it.message) "message" else "activity"
            },
        )
        val (webhooks, tests) = timeline[2].agents
        assertEquals(SubagentState.RUNNING, webhooks.state)
        assertEquals("Explore", webhooks.type)
        assertEquals("Prompt for Webhooks", webhooks.prompt)
        assertEquals("Reading webhook.rs", webhooks.activity)
        assertEquals("Found three handlers.", webhooks.message)
        assertEquals(listOf("rg -n webhook" to "Terminé"), webhooks.commands())
        assertEquals(SubagentState.DONE, tests.state)
        assertEquals("All billing tests pass.", tests.result)
        assertEquals(1510L, tests.tokens)
        assertEquals(4200L, tests.durationMs)
    }

    @Test
    fun `keeps the agent steps around a team in their order`() {
        val timeline =
            timelineEntries(
                listOf(
                    command("read", "cat README.md"),
                    subagent("item.started", "Review"),
                    command("test", "pnpm test"),
                ),
                chat = true,
            )
        assertEquals(
            listOf("cat README.md", "team", "pnpm test"),
            timeline.map {
                if (it.agents.isNotEmpty()) "team" else presentActivity(it.events.single()).command
            },
        )
    }

    @Test
    fun `clears the previous report and message when a subagent starts a follow-up`() {
        val (agent) =
            teams(
                    listOf(
                        subagent("item.started", "worker"),
                        message("reply", "Old report", parent = "worker"),
                        subagent("item.completed", "worker") {
                            put("status", "completed")
                            put("result", "Old report")
                        },
                        subagent("item.updated", "worker") { put("result", "") },
                    )
                )
                .single()
        assertEquals(SubagentState.RUNNING, agent.state)
        assertEquals("", agent.result)
        assertEquals("", agent.message)
    }

    @Test
    fun `folds early lifecycle activity into its spawn without losing metadata or steps`() {
        val timeline =
            timelineEntries(
                listOf(
                    subagent("item.started", "spawn"),
                    item("item.started") {
                        put("id", "early")
                        put("type", "subagent")
                        put("name", "worker")
                        put("status", "in_progress")
                    },
                    command("read", "pwd", parent = "early"),
                    subagent("item.completed", "spawn") {
                        put("replaces_id", "early")
                        put("name", "worker")
                    },
                    subagent("item.updated", "spawn") {
                        put("replaces_id", "early")
                        put("status", "completed")
                        put("result", "Done")
                    },
                ),
                chat = true,
            )
        assertEquals(1, timeline.size)
        val agent = timeline.single().agents.single()
        assertEquals("Prompt for spawn", agent.prompt)
        assertEquals("Done", agent.result)
        assertEquals("Explore", agent.type)
        assertEquals(listOf("pwd" to "Terminé"), agent.commands())
    }

    @Test
    fun `keeps child step identities stable across parent turns and separate between siblings`() {
        val (first, second) =
            teams(
                    listOf(
                        subagent("item.started", "First"),
                        subagent("item.started", "Second"),
                        command("command", "pwd", parent = "First", running = true),
                        command("command", "ls", parent = "Second"),
                        turn("turn.started"),
                        command("command", "pwd", parent = "First"),
                    )
                )
                .single()
        assertEquals(listOf("pwd" to "Terminé"), first.commands())
        assertEquals(listOf("ls" to "Terminé"), second.commands())
    }

    @Test
    fun `keeps one row per subagent across turns`() {
        val agent =
            teams(
                    listOf(
                        subagent("item.completed", "Background") { put("background", true) },
                        turn("turn.started"),
                        subagent("item.updated", "Background") {
                            put("background", true)
                            put("status", "completed")
                            put("result", "Done later")
                        },
                    )
                )
                .single()
                .single()
        assertEquals("Done later", agent.result)
        assertTrue(agent.background)
    }

    @Test
    fun `keeps unfinished subagents stopped after another execution starts`() {
        val agents =
            teams(
                    listOf(
                        subagent("item.started", "Previous"),
                        command("unfinished", "pnpm test", parent = "Previous", running = true),
                        turn("turn.failed"),
                        event("turn.started", buildJsonObject {}, runId = "next-run"),
                        subagent("item.started", "Previous", runId = "next-run"),
                    )
                )
                .flatten()
        assertEquals(listOf(SubagentState.STOPPED, SubagentState.RUNNING), agents.map { it.state })
        assertEquals(listOf("pnpm test" to "Arrêté"), agents[0].commands())
    }

    @Test
    fun `marks failed and stopped subagents`() {
        val agents =
            teams(
                    listOf(
                        subagent("item.completed", "Broken") {
                            put("status", "failed")
                            put("result", "Permission denied")
                        },
                        subagent("item.completed", "Closed") { put("status", "stopped") },
                    )
                )
                .single()
        assertEquals(listOf(SubagentState.FAILED, SubagentState.STOPPED), agents.map { it.state })
        assertEquals(
            listOf("Permission denied", "Arrêté avant d’avoir rendu compte"),
            agents.map { it.summary(true) },
        )
    }

    @Test
    fun `names Codex subagent tools and keeps orphaned messages readable`() {
        val timeline =
            timelineEntries(
                listOf(
                    item("item.completed") {
                        put("id", "close")
                        put("type", "collab_tool_call")
                        put("tool", "closeAgent")
                        put("prompt", JsonNull)
                        put("status", "completed")
                    },
                    message("lost", "From an unloaded subagent", parent = "missing"),
                )
            )
        assertEquals(
            listOf("Sous-agent fermé", "Message d’un sous-agent"),
            timeline.single().events.map { presentActivity(it).title },
        )
    }

    @Test
    fun `finds the subagents of the current request and what each one does`() {
        val timeline =
            timelineEntries(
                listOf(
                    user("First"),
                    subagent("item.completed", "Earlier") {
                        put("status", "completed")
                        put("result", "## Findings\nAll good")
                    },
                    user("Second"),
                    subagent("item.started", "Webhooks") { put("activity", "Reading webhook.rs") },
                    message("note", "Meanwhile…"),
                    subagent("item.started", "Tests"),
                ),
                chat = true,
            )
        val current = currentSubagents(timeline)
        assertEquals(listOf("Webhooks", "Tests"), current.map { it.name })
        assertEquals(listOf("Reading webhook.rs", "Démarrage…"), current.map { it.summary(true) })
        // Once the run ends, a subagent that never reported shows as stopped.
        assertEquals(SubagentState.STOPPED, current[0].state(false))
        assertEquals("Findings", subagentTeams(timeline).first().single().summary(false))
    }

    @Test
    fun `keeps earlier workers and nested children reachable after another user message`() {
        val timeline =
            timelineEntries(
                listOf(
                    user("First"),
                    subagent("item.started", "Earlier"),
                    subagent("item.completed", "Done") { put("status", "completed") },
                    subagent("item.started", "Nested") { put("parent_id", "Earlier") },
                    user("Keep going"),
                    subagent("item.started", "New"),
                ),
                chat = true,
            )
        assertEquals(listOf("Earlier", "Nested", "New"), currentSubagents(timeline).map { it.name })
        assertEquals(
            listOf(listOf("Earlier", "Done"), listOf("Nested"), listOf("New")),
            subagentTeams(timeline).map { team -> team.map { it.name } },
        )
    }

    @Test
    fun `tells that the agent waits for its subagents`() {
        val events =
            listOf(
                user("Audit"),
                subagent("item.started", "Webhooks"),
                subagent("item.started", "Tests"),
            )
        val both = workingStep(events, "Claude", null)
        assertEquals("En attente de 2 sous-agents", both.title)
        assertEquals("Webhooks · Tests", both.detail)
        assertTrue(both.subagents)

        val one = events + subagent("item.completed", "Tests") { put("status", "completed") }
        assertEquals("En attente d’un sous-agent", workingStep(one, "Claude", null).title)
        assertEquals("Webhooks", workingStep(one, "Claude", null).detail)

        // A subagent's own step does not count as the agent's latest step.
        val child = one + command("grep", "rg -n webhook", parent = "Webhooks", running = true)
        assertEquals("En attente d’un sous-agent", workingStep(child, "Claude", null).title)

        val acting = one + command("lint", "pnpm lint", running = true)
        assertFalse(workingStep(acting, "Claude", null).subagents)
    }

    @Test
    fun `keeps waiting for background work while a background subagent reports progress`() {
        val waiting =
            event(
                "turn.waiting",
                buildJsonObject {
                    put("type", "turn.waiting")
                    putJsonArray("tasks") {
                        addJsonObject {
                            put("id", "task")
                            put("description", "List files")
                        }
                    }
                },
            )
        val events =
            listOf(
                user("List"),
                subagent("item.completed", "List files") { put("background", true) },
                waiting,
            )
        val progress =
            events +
                command("ls", "ls", parent = "List files") +
                subagent("item.updated", "List files") {
                    put("background", true)
                    put("activity", "Running ls")
                }
        assertEquals(listOf("List files"), backgroundWait(progress)?.tasks)
        assertNull(backgroundWait(events + message("msg", "")))
    }

    @Test
    fun `replaces an explicit wait only when every awaited task is represented in the bar`() {
        fun waiting(vararg tasks: Pair<String, String>) =
            event(
                "turn.waiting",
                buildJsonObject {
                    put("type", "turn.waiting")
                    putJsonArray("tasks") {
                        tasks.forEach { (id, description) ->
                            addJsonObject {
                                put("id", id)
                                put("description", description)
                            }
                        }
                    }
                },
            )
        val events =
            listOf(
                subagent("item.completed", "Worker") {
                    put("background", true)
                    put("task_id", "worker-task")
                },
                waiting("worker-task" to "Worker"),
            )
        val agents = currentSubagents(timelineEntries(events, chat = true))
        assertTrue(waitingForSubagents(events, agents))
        assertFalse(
            waitingForSubagents(
                events + waiting("worker-task" to "Worker", "build-task" to "Build"),
                agents,
            )
        )
        assertFalse(waitingForSubagents(listOf(waiting()), agents))
    }

    @Test
    fun `team keys stay stable as subagents report`() {
        val start = listOf(subagent("item.started", "Review"))
        val later =
            start +
                command("grep", "rg -n TODO", parent = "Review") +
                subagent("item.completed", "Review") { put("status", "completed") }
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(start, chat = true))
        val second = keys.stabilize("h", timelineEntries(later, chat = true))
        assertEquals(first.map { it.key }, second.map { it.key })
        assertEquals(SubagentState.DONE, second.single().agents.single().state)
    }

    @Test
    fun `loading older turns preserves subagent identities and team keys`() {
        val older = listOf(turn("turn.started"))
        val recent =
            listOf(
                turn("turn.started"),
                subagent("item.started", "Review"),
                subagent("item.started", "Nested") { put("parent_id", "Review") },
            )
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(recent, chat = true))
        val paged = keys.stabilize("h", timelineEntries(older + recent, chat = true))

        assertEquals(
            subagentTeams(first).flatten().map { it.id },
            subagentTeams(paged).flatten().map { it.id },
        )
        assertEquals(
            first.filter { it.agents.isNotEmpty() }.map { it.key },
            paged.filter { it.agents.isNotEmpty() }.map { it.key },
        )
    }

    @Test
    fun `loading an earlier teammate preserves the team key until history resets`() {
        val older = listOf(message("older", "Earlier reply"), subagent("item.started", "Earlier"))
        val recent = listOf(subagent("item.started", "Review"))
        val keys = TimelineKeys()
        val first = keys.stabilize("h", timelineEntries(recent, chat = true)).single()
        val paged =
            keys.stabilize("h", timelineEntries(older + recent, chat = true)).single {
                it.agents.isNotEmpty()
            }

        assertEquals(first.key, paged.key)
        assertEquals(listOf("Earlier", "Review"), paged.agents.map { it.name })
        val reset =
            keys.stabilize("new-history", timelineEntries(older + recent, chat = true)).single {
                it.agents.isNotEmpty()
            }
        assertFalse(first.key == reset.key)
    }

    @Test
    fun `teams that split after pagination retain distinct identities`() {
        val (earlier, recent) =
            teams(
                    listOf(
                        subagent("item.started", "Earlier"),
                        subagent("item.started", "Review"),
                    )
                )
                .single()
        val keys = SubagentTeamKeys()
        val first = keys.stabilize(listOf(listOf(recent))).single()
        assertEquals(first, keys.stabilize(listOf(listOf(earlier, recent))).single())
        val split = keys.stabilize(listOf(listOf(earlier), listOf(recent)))
        assertEquals(2, split.distinct().size)
        assertEquals(split, keys.stabilize(listOf(listOf(earlier), listOf(recent))))
    }

    @Test
    fun `initials and details read like the web`() {
        assertEquals("CR", subagentInitials("Code review"))
        assertEquals("EX", subagentInitials("explore"))
        assertEquals("·", subagentInitials("—"))
        val agent =
            teams(
                    listOf(
                        subagent("item.completed", "Audit") {
                            put("status", "completed")
                            put("model", "claude-sonnet-5")
                            put("tool_uses", 1)
                            put("tokens", 12)
                        }
                    )
                )
                .single()
                .single()
        assertEquals("claude-sonnet-5 · 1 utilisation d’outil · 12 jetons", agent.details())
    }

    private fun delegated(chatId: String) =
        event(
            "chat.delegated",
            buildJsonObject {
                put("chatId", chatId)
                put("title", "Task $chatId")
                put("agentName", "Reviewer")
                put("provider", "codex")
                put("model", "gpt-5.5")
                put("task", "Review for $chatId")
            },
        )

    @Test
    fun `tasks delegated together form one team followed through their conversations`() {
        val events = listOf(user("Review it"), delegated("a"), delegated("b"), delegated("a"))
        val team = teams(events).single()
        assertEquals(listOf("a", "b"), team.map { it.delegation?.chatId })
        assertEquals("Task a", team.first().name)
        assertEquals("Review for a", team.first().prompt)
        assertEquals("gpt-5.5", team.first().model)
        // A delegated task keeps working after the requester's turn ends.
        assertEquals(SubagentState.RUNNING, team.first().state(active = false))

        fun follow(chat: Chat?) = team.first().followed(chat).let { it.state to it.summary(false) }
        val chat = Chat("a", status = "running", runId = "child")
        assertEquals(SubagentState.RUNNING to "En cours", follow(chat))
        assertEquals(
            SubagentState.RUNNING to "En attente d’un runner",
            follow(chat.copy(status = "idle", runId = null)),
        )
        assertEquals(
            SubagentState.RUNNING to "Pose une question",
            follow(chat.copy(status = "succeeded", pendingQuestions = 1)),
        )
        assertEquals(
            SubagentState.DONE to "2 findings",
            follow(chat.copy(status = "succeeded", preview = "2 findings")),
        )
        assertEquals(
            SubagentState.DONE to "A rendu compte",
            follow(chat.copy(status = "succeeded")),
        )
        assertEquals(
            SubagentState.STOPPED to "Arrêtée",
            follow(chat.copy(status = "cancelled", paused = true)),
        )
        assertEquals(SubagentState.FAILED to "Échec", follow(chat.copy(status = "failed")))
        assertEquals(
            SubagentState.STOPPED to "Arrêtée",
            follow(chat.copy(status = "succeeded", paused = true, pendingQuestions = 1)),
        )
        assertEquals(
            SubagentState.STOPPED to "Arrêtée",
            follow(chat.copy(paused = true, pendingQuestions = 2)),
        )
        assertEquals(SubagentState.STOPPED to "Conversation supprimée", follow(null))
        assertEquals("", team.first().elapsed(active = true, now = 10_000))
    }

    @Test
    fun `interleaved delegation calls form one team while failed calls stay visible`() {
        fun call(type: String, id: String, qualified: Boolean, status: String) =
            item(type) {
                put("id", id)
                put("type", "mcp_tool_call")
                put(
                    "tool",
                    if (qualified) "mcp__cairn_workspace__delegate_task" else "delegate_task",
                )
                if (!qualified) put("server", "cairn_workspace")
                put("status", status)
            }
        val events =
            listOf(
                user("Review it"),
                call("item.started", "call-a", true, "in_progress"),
                delegated("a"),
                call("item.completed", "call-a", true, "completed"),
                call("item.started", "call-b", false, "in_progress"),
                delegated("b"),
                call("item.completed", "call-b", false, "completed"),
            )
        val timeline = timelineEntries(events, chat = true)
        assertEquals(2, timeline.size)
        assertEquals(listOf("a", "b"), timeline.last().agents.map { it.delegation?.chatId })
        val failed = call("item.completed", "call-c", true, "failed")
        val withFailure = timelineEntries(events + failed, chat = true)
        assertTrue(withFailure.last().events.contains(failed))
        assertTrue(presentActivity(failed).failed)
    }

    @Test
    fun `earlier delegated tasks stay with the request that started them`() {
        val timeline =
            timelineEntries(listOf(user("First"), delegated("a"), user("Second")), chat = true)
        assertTrue(currentSubagents(timeline).isEmpty())
        val followed = timeline.followingDelegations(listOf(Chat("a", status = "succeeded")))
        assertEquals(
            SubagentState.DONE,
            followed.single { it.agents.isNotEmpty() }.agents.single().state,
        )
    }
}
