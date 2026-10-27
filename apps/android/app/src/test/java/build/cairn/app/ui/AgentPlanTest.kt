package build.cairn.app.ui

import build.cairn.app.data.RunEvent
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

/** Mirrors the web's agent-plan cases. */
class AgentPlanTest {
    private var serial = 0L

    private fun event(type: String, item: JsonObject? = null, text: String = ""): RunEvent {
        val id = ++serial
        return RunEvent(
            id = id,
            createdAt = id * 1000,
            type = type,
            text = text,
            payload = item?.let { JsonObject(mapOf("item" to it)) },
            runId = "chat",
        )
    }

    private fun plan(
        id: String,
        vararg steps: Pair<String, String>,
        body: JsonObjectBuilder.() -> Unit = {},
    ) =
        event(
            "item.updated",
            buildJsonObject {
                put("id", id)
                put("type", "todo_list")
                putJsonArray("items") {
                    steps.forEach { (text, status) ->
                        addJsonObject {
                            put("text", text)
                            put("status", status)
                            put("completed", status == "completed")
                        }
                    }
                }
                body()
            },
        )

    private fun command(command: String) =
        event(
            "item.completed",
            buildJsonObject {
                put("id", "cmd-$serial")
                put("type", "command_execution")
                put("command", command)
                put("exit_code", 0)
                put("status", "completed")
            },
        )

    private fun user(text: String) =
        RunEvent(
            id = ++serial,
            createdAt = serial * 1000,
            type = "chat.user",
            text = text,
            payload = kotlinx.serialization.json.JsonObject(mapOf("text" to JsonPrimitive(text))),
        )

    private fun kinds(timeline: List<TimelineEntry>) = timeline.map {
        when {
            it.message -> "message"
            it.plan != null -> "plan"
            it.planUpdate != null -> "plan-update"
            it.agents.isNotEmpty() -> "team"
            else -> "group"
        }
    }

    @Test
    fun `steps keep their states and agents that report only completion still read`() {
        val steps =
            planSteps(
                buildJsonObject {
                    putJsonArray("items") {
                        addJsonObject {
                            put("text", "Explore")
                            put("status", "completed")
                        }
                        addJsonObject {
                            put("text", "Migrate")
                            put("status", "in_progress")
                            put("active_text", "Migrating")
                        }
                        addJsonObject {
                            put("text", "Document")
                            put("status", "cancelled")
                        }
                        addJsonObject {
                            put("text", "Check")
                            put("completed", false)
                        }
                        addJsonObject {
                            put("text", "Legacy done")
                            put("completed", true)
                        }
                        addJsonObject { put("text", "  ") }
                    }
                }
            )

        assertEquals(
            listOf(
                PlanStepState.DONE,
                PlanStepState.ACTIVE,
                PlanStepState.CANCELLED,
                PlanStepState.PENDING,
                PlanStepState.DONE,
            ),
            steps.map { it.state },
        )
        assertEquals("Migrating", steps[1].activeText)
        // Dropped steps leave the plan's total.
        assertEquals(PlanProgress(2, 4), steps.progress())
        assertEquals(1, steps.currentStep()?.index)
        assertTrue(steps.currentStep()!!.active)
    }

    @Test
    fun `an update names what it finished started added and dropped`() {
        val before =
            listOf(
                PlanStep("A", "", PlanStepState.ACTIVE),
                PlanStep("B", "", PlanStepState.PENDING),
                PlanStep("C", "", PlanStepState.PENDING),
            )
        val after =
            listOf(
                PlanStep("A", "", PlanStepState.DONE),
                PlanStep("B", "", PlanStepState.ACTIVE),
                PlanStep("C", "", PlanStepState.CANCELLED),
                PlanStep("D", "", PlanStepState.PENDING),
            )

        assertEquals(
            PlanChange(listOf("A"), listOf("B"), listOf("D"), listOf("C")),
            planChange(before, after),
        )
        assertNull(planChange(after, after))
        // Claude Code drops a step by leaving it out; a finished step that leaves is not news.
        val trimmed =
            listOf(PlanStep("A", "", PlanStepState.DONE), PlanStep("D", "", PlanStepState.ACTIVE))
        assertEquals(
            PlanChange(started = listOf("D"), cancelled = listOf("B")),
            planChange(after, trimmed),
        )
        assertTrue(continuesPlan(before, after))
        assertFalse(continuesPlan(before, listOf(PlanStep("Unrelated", "", PlanStepState.PENDING))))
    }

    @Test
    fun `the plan shows once where it was written then each change where it happened`() {
        val timeline =
            timelineEntries(
                listOf(
                    user("Add session expiry"),
                    plan("plan:turn", "Explore" to "in_progress", "Migrate" to "pending") {
                        put("explanation", "Data model first.")
                    },
                    command("rg -n session src"),
                    plan(
                        "plan:turn",
                        "Explore" to "completed",
                        "Migrate" to "in_progress",
                        "Check" to "pending",
                    ),
                    // Claude Code repeats the list when its call completes: nothing changed.
                    plan(
                        "plan:turn",
                        "Explore" to "completed",
                        "Migrate" to "in_progress",
                        "Check" to "pending",
                    ),
                    command("cargo test"),
                    plan(
                        "plan:turn",
                        "Explore" to "completed",
                        "Migrate" to "completed",
                        "Check" to "completed",
                    ),
                ),
                chat = true,
            )

        assertEquals(
            listOf("message", "plan", "group", "plan-update", "group", "plan-update"),
            kinds(timeline),
        )
        val card = timeline[1]
        assertEquals(List(3) { PlanStepState.DONE }, card.plan!!.steps.map { it.state })
        assertEquals("Data model first.", card.plan.explanation)
        assertEquals(
            listOf(
                PlanUpdate(
                    card.key,
                    PlanChange(listOf("Explore"), listOf("Migrate"), listOf("Check")),
                    PlanProgress(1, 3),
                ),
                PlanUpdate(card.key, PlanChange(listOf("Migrate", "Check")), PlanProgress(3, 3)),
            ),
            timeline.mapNotNull { it.planUpdate },
        )
        // Keys survive the stabilizer that keeps tool groups anchored.
        assertEquals(
            timeline.map { it.key },
            TimelineKeys().stabilize("h", timeline).map { it.key },
        )
    }

    @Test
    fun `one card stays when an agent repeats the list that finished the plan`() {
        val timeline =
            timelineEntries(
                listOf(
                    plan("todo-1", "Explore" to "in_progress"),
                    plan("todo-2", "Explore" to "completed"),
                    // Claude Code repeats the list when the call completes, once the plan is done.
                    plan("todo-2", "Explore" to "completed"),
                ),
                chat = true,
            )

        assertEquals(listOf("plan", "plan-update"), kinds(timeline))
    }

    @Test
    fun `a finished or replaced plan gives way to a new one`() {
        val timeline =
            timelineEntries(
                listOf(
                    plan("first", "Explore" to "completed"),
                    plan("second", "Explore" to "in_progress"),
                    plan("third", "Something else" to "pending"),
                ),
                chat = true,
            )

        assertEquals(listOf("plan", "plan", "plan"), kinds(timeline))
    }

    @Test
    fun `metadata refreshes without another card or delta for an unchanged plan`() {
        for (status in listOf("in_progress", "completed")) {
            fun update(activity: String, explanation: String) =
                plan("plan") {
                    put("explanation", explanation)
                    putJsonArray("items") {
                        addJsonObject {
                            put("text", "Implement")
                            put("status", status)
                            put("active_text", activity)
                        }
                    }
                }

            val initial = update("Implementing A", "Initial approach.")
            val revised = update("Implementing B", "Revised approach.")
            val repeated = update("Implementing B", "Revised approach.")
            val timeline = timelineEntries(listOf(initial, revised, repeated), chat = true)

            assertEquals(listOf("plan"), kinds(timeline))
            val card = timeline.single().plan!!
            assertEquals("Implementing B", card.steps.single().activeText)
            assertEquals("Revised approach.", card.explanation)
            assertEquals(initial.createdAt, card.time)
            assertEquals(revised.createdAt, card.updatedAt)
            if (status == "in_progress")
                assertEquals("Implementing B", currentPlan(timeline)?.steps?.single()?.activeText)
        }
    }

    @Test
    fun `empty and failed updates are left out and a subagent plan stays among its steps`() {
        val timeline =
            timelineEntries(
                listOf(
                    plan("announced"),
                    plan("failed", "Explore" to "pending") { put("status", "failed") },
                    event(
                        "item.started",
                        buildJsonObject {
                            put("id", "agent")
                            put("type", "subagent")
                            put("name", "Review")
                            put("status", "in_progress")
                        },
                    ),
                    plan("child", "Review the keyboard" to "in_progress") {
                        put("parent_id", "agent")
                    },
                ),
                chat = true,
            )

        assertEquals(listOf("team"), kinds(timeline))
    }

    @Test
    fun `a child plan stays among tools when its parent is outside the loaded history`() {
        val child = plan("child", "Review" to "in_progress") { put("parent_id", "older-parent") }
        val childOnly = timelineEntries(listOf(child), chat = true)

        assertEquals(listOf("group"), kinds(childOnly))
        assertEquals(listOf(child), childOnly.single().events)
        assertNull(currentPlan(childOnly))

        val timeline =
            timelineEntries(
                listOf(plan("root", "Implement" to "in_progress"), child),
                chat = true,
            )
        assertEquals(listOf("plan", "group"), kinds(timeline))
        assertEquals(listOf("Implement"), currentPlan(timeline)?.steps?.map { it.text })
    }

    @Test
    fun `the current request plan stays beside the composer until it is finished`() {
        val working =
            listOf(
                user("Add session expiry"),
                plan("plan", "Explore" to "completed", "Migrate" to "in_progress"),
            )
        assertEquals(
            listOf("Explore", "Migrate"),
            currentPlan(timelineEntries(working, chat = true))?.steps?.map { it.text },
        )

        // A new message leaves the earlier plan behind until the agent changes it again.
        val followUp = working + user("Also update the docs")
        assertNull(currentPlan(timelineEntries(followUp, chat = true)))
        val resumed =
            followUp +
                plan(
                    "plan",
                    "Explore" to "completed",
                    "Migrate" to "completed",
                    "Docs" to "in_progress",
                )
        assertEquals(
            listOf(PlanStepState.DONE, PlanStepState.DONE, PlanStepState.ACTIVE),
            currentPlan(timelineEntries(resumed, chat = true))?.steps?.map { it.state },
        )
        // The new request gets its own card instead of rewriting the earlier one further up.
        assertEquals(
            listOf("message", "plan", "message", "plan"),
            kinds(timelineEntries(resumed, chat = true)),
        )

        val finished = working + plan("plan", "Explore" to "completed", "Migrate" to "completed")
        assertNull(currentPlan(timelineEntries(finished, chat = true)))
    }
}
