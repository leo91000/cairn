package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

private const val IMMEDIATE_WAIT_RESULT = "Wait condition already completed"

class ChatWaitTest {
    // A plugin's condition: only the plugin knows what its fields mean.
    private val checks = buildJsonObject {
        put("type", "github_pr_checks")
        put("pluginId", "github")
        put("projectId", "project")
        put("prNumber", 193)
        put("headSha", "0b2ddce3a1b2c3d4e5f60718293a4b5c6d7e8f90")
    }
    // How the plugin words it while it waits.
    private val display = buildJsonObject {
        put("title", "Waiting for required checks on PR #193")
        put("short", "PR #193 checks")
        putJsonArray("details") { add("0b2ddce") }
    }

    private fun wakeup(condition: JsonObject? = null, wakeAt: Long = 1_800_000) =
        ChatWakeup(
            "wake",
            "first",
            "Merger après la CI",
            1800,
            0,
            wakeAt,
            condition = condition,
            display = display.takeIf { condition != null },
        )

    private var id = 0L

    private fun event(type: String, payload: JsonObject, text: String = "") =
        (++id).let {
            RunEvent(
                id = it,
                createdAt = it * 60_000,
                type = type,
                text = text,
                payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
            )
        }

    private fun ChatWakeup.json() = wireJson.encodeToJsonElement(this).jsonObject

    private fun toolCall(
        tool: String,
        server: String,
        error: Boolean = false,
        arguments: JsonElement? = null,
    ) =
        event(
            "item.completed",
            buildJsonObject {
                putJsonObject("item") {
                    put("id", "tool-$id")
                    put("type", "mcp_tool_call")
                    put("tool", tool)
                    put("server", server)
                    put("status", "completed")
                    if (error) putJsonObject("error") { put("message", "Pending user input") }
                    if (arguments != null) put("arguments", arguments)
                }
            },
        )

    @Test
    fun `time reached completes a timer without an external timeout warning`() {
        val result = buildJsonObject {
            put("state", "completed")
            put("type", "time")
            put("wakeAt", 1000)
        }
        assertEquals(WaitOutcome(WaitTone.NEUTRAL, "Échéance atteinte", ""), waitOutcome(result))
        assertEquals("Heure prévue", observedTarget(result))
    }

    @Test
    fun `names what the conversation waits for and counts down to it`() {
        assertEquals(
            WaitTarget(
                WaitKind.CONDITION,
                "Waiting for required checks on PR #193",
                "PR #193 checks",
                listOf("0b2ddce"),
            ),
            waitTarget(wakeup(checks)),
        )
        // A plugin that gives no wording, or an unexpected one, still reads as a condition.
        assertEquals(
            WaitTarget(WaitKind.CONDITION, "Attente d’une condition", "Condition", emptyList()),
            waitTarget(wakeup(checks).copy(display = null)),
        )
        assertEquals(
            WaitTarget(WaitKind.CONDITION, "Attente d’une condition", "Condition", emptyList()),
            waitTarget(
                wakeup(checks)
                    .copy(
                        display =
                            buildJsonObject {
                                put("title", 3)
                                put("details", "abc")
                            }
                    )
            ),
        )
        val run = buildJsonObject {
            put("type", "run_finished")
            put("runId", "run")
        }
        assertEquals(WaitKind.RUN, waitTarget(wakeup(run)).kind)
        assertTrue(waitTarget(wakeup()).short.startsWith("Reprise à "))
        // Seen only in a run's history, a wait reads as what was scheduled.
        assertTrue(recordedTitle(wakeup()).startsWith("Reprise programmée à "))
        assertEquals("Attente programmée · PR #193 checks", recordedTitle(wakeup(checks)))
        // The plugin's words survive the wakeup's round trip through events.
        assertEquals(
            "PR #193 checks",
            wireJson.decodeFromJsonElement<ChatWakeup>(wakeup(checks).json()).let {
                waitTarget(it).short
            },
        )
        assertEquals("45 s", timeLeft(45_000))
        assertEquals("2 min", timeLeft(61_000))
        assertEquals("1 h 05", timeLeft(65 * 60_000L))
        assertEquals("dans 20 min", waitCountdown(wakeup(), 600_000))
        assertEquals("20 min max", waitCountdown(wakeup(checks), 600_000))
        assertEquals("Reprise imminente", waitCountdown(wakeup(), 2_000_000))
    }

    @Test
    fun `says how the wait ended`() {
        // The plugin's own words come first.
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "Required checks failed", "lint"),
            waitOutcome(
                buildJsonObject {
                    put("state", "completed")
                    put("type", "github_pr_checks")
                    put("pluginId", "github")
                    put("conclusion", "failure")
                    putJsonObject("display") {
                        put("tone", "error")
                        put("title", "Required checks failed")
                        put("detail", "lint")
                    }
                }
            ),
        )
        assertEquals(
            WaitOutcome(WaitTone.NEUTRAL, "PR #193 was merged", ""),
            waitOutcome(
                buildJsonObject {
                    put("state", "closed")
                    put("pluginId", "github")
                    putJsonObject("display") {
                        put("tone", "sparkly")
                        put("title", "PR #193 was merged")
                    }
                }
            ),
        )
        assertEquals(WaitTone.NEUTRAL, waitOutcome(null).tone)
        // Generic outcomes, without plugin words.
        assertEquals(
            WaitOutcome(WaitTone.WARNING, "Échéance atteinte sans résultat", ""),
            waitOutcome(buildJsonObject { put("state", "timed_out") }),
        )
        assertEquals(
            WaitOutcome(WaitTone.WARNING, "Résultat indisponible", "No access"),
            waitOutcome(
                buildJsonObject {
                    put("state", "unavailable")
                    put("message", "No access")
                }
            ),
        )
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "L’exécution a échoué", ""),
            waitOutcome(
                buildJsonObject {
                    put("state", "completed")
                    put("type", "run_finished")
                    put("status", "failed")
                }
            ),
        )
        // Otherwise its conclusion, when it has one.
        fun concluded(conclusion: String) =
            waitOutcome(
                buildJsonObject {
                    put("state", "completed")
                    put("type", "github_workflow")
                    put("pluginId", "github")
                    put("conclusion", conclusion)
                }
            )
        assertEquals(WaitOutcome(WaitTone.OK, "Réussi", ""), concluded("success"))
        assertEquals(WaitOutcome(WaitTone.ERROR, "Failure", ""), concluded("failure"))
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "Startup failure", ""),
            concluded("startup_failure"),
        )
        assertEquals(
            WaitOutcome(WaitTone.WARNING, "Action required", ""),
            concluded("action_required"),
        )
        assertEquals(
            WaitOutcome(WaitTone.NEUTRAL, "Attente terminée", ""),
            waitOutcome(
                buildJsonObject {
                    put("state", "superseded")
                    put("pluginId", "github")
                }
            ),
        )
    }

    @Test
    fun `mission waits keep one live card without a conversation pause`() {
        val scheduled = wakeup().copy(messageId = null)
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "time")
        }
        val entries =
            timelineEntries(
                listOf(
                    event("run.wait", scheduled.json()),
                    event(
                        "run.wakeup",
                        JsonObject(scheduled.json() + ("observation" to observation)),
                    ),
                )
            )
        assertEquals(1, entries.count { it.wait != null })
        assertEquals(observation, entries.single().wait?.observation)
        assertTrue(entries.single().wait?.resumedAt != null)
        val state =
            waitProgress(ActivityWait(scheduled), LiveWait(scheduled, false, canPause = false))
        assertTrue(state.pending)
        assertFalse(state.pausable)
        assertTrue(
            build.cairn.app.data
                .Run(
                    id = "mission",
                    status = "waiting",
                    taskId = "",
                )
                .active
        )
    }

    @Test
    fun `folds the wait tool and its events into one card that ends with the resumption`() {
        val scheduled = wakeup(checks)
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "github_pr_checks")
            put("conclusion", "success")
        }
        val entries =
            timelineEntries(
                listOf(
                    toolCall("wait_for", "cairn_workspace"),
                    event("chat.wait", scheduled.json()),
                    event("chat.wait_ready", observation),
                    event(
                        "chat.wakeup",
                        JsonObject(scheduled.json() + ("observation" to observation)),
                    ),
                ),
                chat = true,
            )
        assertEquals(listOf("wait:wake"), entries.map { it.key })
        assertEquals(
            ActivityWait(scheduled, observation, observed = true, resumedAt = 240_000),
            entries.single().wait,
        )
    }

    @Test
    fun `places a scheduled wait at the end of its turn, after what the agent did once it called`() {
        val scheduled = wakeup(checks)
        fun message(text: String) =
            event(
                "item.completed",
                buildJsonObject {
                    putJsonObject("item") {
                        put("id", "message-$id")
                        put("type", "agent_message")
                        put("text", text)
                    }
                },
            )
        fun shape(entries: List<TimelineEntry>) = entries.map {
            when {
                it.wait != null -> "wait"
                it.message ->
                    it.events.single().item().string("text").ifBlank { it.events.single().text }
                else -> "activity"
            }
        }
        val turn =
            listOf(
                event("turn.started", buildJsonObject {}),
                message("J’ai poussé le correctif."),
                toolCall("wait_for", "cairn_workspace"),
                event("chat.wait", scheduled.json()),
                event(
                    "item.completed",
                    buildJsonObject {
                        putJsonObject("item") {
                            put("id", "reasoning-$id")
                            put("type", "reasoning")
                            put("text", "Résumer")
                        }
                    },
                ),
                message("La CI tourne ; je reprends à la fin."),
            )
        // While the turn streams, the card stays last.
        assertEquals(
            listOf(
                "J’ai poussé le correctif.",
                "activity",
                "La CI tourne ; je reprends à la fin.",
                "wait",
            ),
            shape(timelineEntries(turn, chat = true)),
        )
        val ended =
            timelineEntries(
                turn +
                    event("turn.completed", buildJsonObject {}) +
                    event("chat.user", buildJsonObject { put("text", "Merci") }, "Merci"),
                chat = true,
            )
        assertEquals(
            listOf(
                "J’ai poussé le correctif.",
                "activity",
                "La CI tourne ; je reprends à la fin.",
                "wait",
                "Merci",
            ),
            shape(ended),
        )
        assertEquals("cancelled", ended[3].wait?.ended)
        // A wait the agent replaced stays where the later call replaced it.
        val replaced =
            timelineEntries(
                listOf(
                    event("chat.wait", scheduled.json()),
                    message("Un minuteur suffit."),
                    event("chat.sleep", scheduled.copy(id = "later", condition = null).json()),
                    message("Je patiente."),
                ),
                chat = true,
            )
        assertEquals(
            listOf("Un minuteur suffit.", "wait", "Je patiente.", "wait"),
            shape(replaced),
        )
        assertEquals("replaced", replaced[1].wait?.ended)
        // A result that was already available stays at its call, whatever its wording, and the
        // wait it replaced is placed before it.
        val ready = buildJsonObject {
            put("state", "completed")
            put("type", "run_finished")
            put("status", "succeeded")
            put("immediate", true)
        }
        val immediate =
            timelineEntries(
                listOf(
                    event("turn.started", buildJsonObject {}),
                    event("chat.wait", scheduled.json()),
                    message("L’exécution est peut-être finie."),
                    event("chat.wait_ready", ready, "Résultat prêt"),
                    message("Elle est finie ; je merge."),
                ),
                chat = true,
            )
        assertEquals(
            listOf(
                "L’exécution est peut-être finie.",
                "wait",
                "wait",
                "Elle est finie ; je merge.",
            ),
            shape(immediate),
        )
        assertEquals(scheduled, immediate[1].wait?.wakeup)
        assertEquals("replaced", immediate[1].wait?.ended)
        assertEquals(null, immediate[2].wait?.wakeup)
        assertEquals(ready, immediate[2].wait?.observation)
    }

    @Test
    fun `paginated ready and resumption events share one card`() {
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "github_pr_checks")
            put("conclusion", "success")
        }
        val scheduled = wakeup(checks).copy(observation = observation)
        val ready = event("chat.wait_ready", observation).copy(text = "Wait condition resolved")
        val entries =
            timelineEntries(listOf(ready, event("chat.wakeup", scheduled.json())), chat = true)
        assertEquals(listOf("wait:wake"), entries.map { it.key })
        assertEquals(observation, entries.single().wait?.observation)
        assertNotNull(entries.single().wait?.resumedAt)
        val scheduleOnly = event("chat.wait", wakeup(checks).json())
        assertEquals(
            observation,
            timelineEntries(listOf(scheduleOnly), chat = true, pendingWakeup = scheduled)
                .single()
                .wait
                ?.observation,
        )
        val pending = timelineEntries(listOf(ready), chat = true, pendingWakeup = scheduled)
        assertEquals(listOf("wait:wake"), pending.map { it.key })
        assertEquals(observation, pending.single().wait?.observation)
        assertTrue(
            timelineEntries(emptyList(), chat = true, pendingWakeup = scheduled)
                .single()
                .wait!!
                .finished
        )
    }

    @Test
    fun `an immediately completed replacement does not resolve the previous wait`() {
        val observation = buildJsonObject {
            put("immediate", true)
            put("state", "completed")
            put("type", "github_workflow")
            put("runId", 99)
            put("conclusion", "success")
        }
        val entries =
            timelineEntries(
                listOf(
                    event("chat.wait", wakeup(checks).json()),
                    event("chat.wait_ready", observation)
                        .copy(text = "Wait condition already completed"),
                ),
                chat = true,
            )
        assertEquals(2, entries.size)
        assertEquals("replaced", entries[0].wait?.ended)
        assertNull(entries[0].wait?.observation)
        assertNull(entries[1].wait?.wakeup)
        assertEquals(observation, entries[1].wait?.observation)
    }

    @Test
    fun `older resolved waits do not complete a later pending replacement`() {
        val observation = buildJsonObject {
            put("state", "completed")
            put("type", "github_pr_checks")
            put("conclusion", "success")
        }
        val ready = event("chat.wait_ready", observation).copy(text = "Wait condition resolved")
        val later = wakeup(checks).copy(id = "later", createdAt = ready.createdAt + 1)
        val entries =
            timelineEntries(
                    listOf(
                        ready,
                        event("chat.user", buildJsonObject { put("text", "Another task") }),
                        event("chat.wait", later.json()),
                    ),
                    chat = true,
                    pendingWakeup = later,
                )
                .filter { it.wait != null }
        assertEquals(2, entries.size)
        assertNull(entries[0].wait?.wakeup)
        assertEquals(observation, entries[0].wait?.observation)
        assertEquals("wait:later", entries[1].key)
        assertNull(entries[1].wait?.observation)
    }

    @Test
    fun `keeps a failed wait call among the tools and recognises every provider's name`() {
        val entries =
            timelineEntries(
                listOf(
                    toolCall("mcp__cairn_workspace__sleep", "Claude Code"),
                    toolCall("cairn_workspace_sleep", "OpenCode"),
                    toolCall("sleep", "cairn_workspace", error = true),
                ),
                chat = true,
            )
        assertEquals(1, entries.size)
        assertNull(entries.single().wait)
        assertEquals(1, entries.single().events.size)
    }

    @Test
    fun `marks a wait a message cancelled, and an immediate result as its own card`() {
        val entries =
            timelineEntries(
                listOf(
                    event("chat.sleep", wakeup().json()),
                    event(
                        "chat.user",
                        buildJsonObject {
                            put("messageId", "next")
                            put("text", "En fait, maintenant")
                        },
                    ),
                    event(
                        "chat.wait_ready",
                        buildJsonObject {
                            put("state", "completed")
                            put("type", "run_finished")
                            put("status", "succeeded")
                        },
                        IMMEDIATE_WAIT_RESULT,
                    ),
                ),
                chat = true,
            )
        assertEquals(listOf(false, true, false), entries.map { it.message })
        assertEquals("cancelled", entries[0].wait?.ended)
        assertNull(entries[2].wait?.wakeup)
        assertTrue(entries[2].wait!!.finished)
    }

    @Test
    fun `keeps an immediate result apart from the wait it replaced`() {
        val replaced =
            wakeup(
                buildJsonObject {
                    put("type", "run_finished")
                    put("runId", "first")
                }
            )
        val immediate = buildJsonObject {
            put("immediate", true)
            put("state", "completed")
            put("type", "run_finished")
            put("status", "succeeded")
        }
        val entries =
            timelineEntries(
                listOf(
                    event("chat.wait", replaced.json()),
                    event("chat.wait_ready", immediate, IMMEDIATE_WAIT_RESULT),
                ),
                chat = true,
            )
        assertEquals(
            listOf(
                ActivityWait(replaced, ended = "replaced"),
                ActivityWait(null, immediate, observed = true),
            ),
            entries.map { it.wait },
        )
        fun call(type: String, callId: String, reason: String) =
            event(
                type,
                buildJsonObject {
                    putJsonObject("item") {
                        put("id", callId)
                        put("type", "mcp_tool_call")
                        put("tool", "wait_for")
                        put("server", "cairn_workspace")
                        put("status", if (type == "item.started") "in_progress" else "completed")
                        putJsonObject("arguments") { put("reason", reason) }
                    }
                },
            )
        // Recorded with the result, the reason no longer depends on when the call is reported.
        fun ready(reason: String) =
            event(
                "chat.wait_ready",
                JsonObject(immediate + ("reason" to JsonPrimitive(reason))),
                IMMEDIATE_WAIT_RESULT,
            )
        val recorded =
            timelineEntries(
                listOf(
                    ready("Premier"),
                    call("item.started", "c1", "Premier"),
                    ready("Second"),
                    call("item.started", "c2", "Second"),
                ),
                chat = true,
            )
        assertEquals(listOf("Premier", "Second"), recorded.mapNotNull { it.wait?.reason })
        // A resumption whose scheduling is unloaded keeps one card, even if it repeats.
        val resumed =
            timelineEntries(
                listOf(
                    event("chat.wakeup", wakeup().json()),
                    event("chat.wakeup", wakeup().json()),
                ),
                chat = true,
            )
        assertEquals(listOf("wait:wake"), resumed.map { it.key })
    }

    @Test
    fun `says when an overdue or answered wait resumes and ignores a failed turn's wakeup`() {
        assertEquals("Reprise dans 20 min", waitStatus(wakeup(), 600_000))
        assertEquals("Résultat attendu · 20 min max", waitStatus(wakeup(checks), 600_000))
        assertEquals("Reprise imminente", waitStatus(wakeup(), 2_000_000))
        assertEquals(
            "Reprise imminente",
            waitStatus(
                wakeup(checks).copy(observation = buildJsonObject { put("state", "completed") }),
                600_000,
            ),
        )
        val waiting =
            Chat(
                id = "chat",
                status = "succeeded",
                run =
                    Run(
                        id = "run",
                        status = "succeeded",
                        taskId = "",
                    ),
                wakeup = wakeup(),
            )
        assertEquals(wakeup(), liveWakeup(waiting))
        assertNull(
            liveWakeup(
                waiting.copy(
                    run =
                        Run(
                            id = "run",
                            status = "failed",
                            taskId = "",
                        )
                )
            )
        )
        assertNull(liveWakeup(waiting.copy(lifecycle = "trash")))
    }

    @Test
    fun `cancels the wait of a run that ends failed, not of a retried turn, and lists answered waits first`() {
        val retried =
            timelineEntries(
                listOf(
                    event("chat.sleep", wakeup().json()),
                    event(
                        "turn.failed",
                        buildJsonObject {
                            putJsonObject("error") { put("message", "Provider unavailable") }
                        },
                    ),
                ),
                chat = true,
            )
        assertNull(retried.first().wait?.ended)
        val failed =
            timelineEntries(
                listOf(
                    event("chat.sleep", wakeup().json()),
                    event("status", buildJsonObject {}, "failed"),
                ),
                chat = true,
            )
        assertEquals("cancelled", failed.first().wait?.ended)
        val done =
            Run(
                id = "run",
                status = "succeeded",
                taskId = "",
            )
        val answered =
            wakeup(checks, 9000).copy(observation = buildJsonObject { put("state", "completed") })
        val feed =
            buildFeed(
                listOf(
                    Chat(
                        id = "sooner",
                        status = "succeeded",
                        run = done,
                        wakeup = wakeup(wakeAt = 5000),
                    ),
                    Chat(id = "answered", status = "succeeded", run = done, wakeup = answered),
                ),
                emptyList(),
                emptyList(),
            )
        assertEquals(listOf("chat:answered", "chat:sooner"), feed.waiting.map { it.key })
        assertEquals("2 reprises prévues", feedSummary(feed))
    }

    @Test
    fun `trusts the live wakeup over the history and keeps a paused or answered wait pausable`() {
        val pending = wakeup()
        val live = LiveWait(pending, false)
        // Whatever the history says, the live wakeup is authoritative: the conversation still
        // waits.
        val retried = waitProgress(ActivityWait(pending, ended = "cancelled"), live)
        assertTrue(retried.current && retried.pending && retried.pausable)
        assertFalse(retried.cancelled)
        // A stop leaves no event: the live state alone says it is over.
        val stopped = waitProgress(ActivityWait(pending), LiveWait(null, false))
        assertTrue(stopped.cancelled)
        assertFalse(stopped.pending || stopped.pausable)
        val paused = waitProgress(ActivityWait(pending), LiveWait(pending, true))
        assertTrue(paused.paused)
        assertFalse(paused.pending || paused.pausable)
        val result = buildJsonObject { put("state", "completed") }
        val answered =
            waitProgress(ActivityWait(pending), LiveWait(pending.copy(observation = result), false))
        assertTrue(answered.finished && answered.pausable)
        assertFalse(answered.pending)
        assertEquals(result, answered.observation)
        val replaced =
            waitProgress(
                ActivityWait(pending, ended = "replaced"),
                LiveWait(pending.copy(id = "next"), false),
            )
        assertTrue(replaced.replaced)
        assertFalse(replaced.cancelled)
        // Without live state, as in a run's history, only the history speaks.
        assertTrue(waitProgress(ActivityWait(pending, ended = "cancelled"), null).cancelled)
        assertEquals(
            "PR #193 checks",
            observedTarget(
                buildJsonObject {
                    put("type", "github_pr_checks")
                    put("pluginId", "github")
                    putJsonObject("display") {
                        put("title", "Checks passed")
                        put("short", "PR #193 checks")
                    }
                }
            ),
        )
        assertEquals(
            "Une condition",
            observedTarget(
                buildJsonObject {
                    put("type", "github_pr_checks")
                    put("prNumber", 193)
                }
            ),
        )
        assertEquals(
            "Autre exécution",
            observedTarget(buildJsonObject { put("type", "run_finished") }),
        )
        assertEquals("", observedTarget(buildJsonObject { put("state", "timed_out") }))
        assertEquals("≤ 10 min", waitLeft(600_000, condition = true, resolved = false, now = 0))
        assertEquals("< 1 min", waitLeft(45_000, condition = false, resolved = false, now = 0))
        assertEquals("< 1 min", waitLeft(45_000, condition = true, resolved = false, now = 0))
        // Until the turn that scheduled it ends, the conversation is not paused from the card.
        val running = waitProgress(ActivityWait(pending), live.copy(settled = false))
        assertTrue(running.pending)
        assertFalse(running.pausable)
        assertEquals("bientôt", waitLeft(600_000, condition = false, resolved = true, now = 0))
        assertEquals(
            "bientôt",
            waitLeft(600_000, condition = false, resolved = false, now = 700_000),
        )
    }

    @Test
    fun `lists waiting conversations apart, soonest first, unless paused`() {
        val done =
            Run(
                id = "run",
                status = "succeeded",
                taskId = "",
            )
        val feed =
            buildFeed(
                listOf(
                    Chat(
                        id = "later",
                        status = "succeeded",
                        run = done,
                        updatedAt = 3,
                        unread = true,
                        finishedAt = 1,
                        wakeup = wakeup(checks, 9000),
                    ),
                    Chat(
                        id = "sooner",
                        status = "succeeded",
                        run = done,
                        updatedAt = 2,
                        wakeup = wakeup(wakeAt = 5000),
                    ),
                    Chat(
                        id = "paused",
                        status = "succeeded",
                        run = done,
                        paused = true,
                        wakeup = wakeup(),
                    ),
                ),
                emptyList(),
                emptyList(),
            )
        assertEquals(listOf("chat:sooner", "chat:later"), feed.waiting.map { it.key })
        assertTrue(feed.forYou.isEmpty())
        assertEquals("PR #193 checks", feed.waiting[1].subtitle)
        assertTrue(feed.waiting[1].condition)
        assertEquals(listOf("chat:paused"), feed.recent.map { it.key })
        assertEquals(feed.keys(), listOf("chat:sooner", "chat:later", "chat:paused"))
    }

    @Test
    fun `lists a mission that scheduled its continuation with waiting conversations`() {
        val done =
            Run(
                id = "run",
                status = "succeeded",
                taskId = "",
            )
        val feed =
            buildFeed(
                listOf(
                    Chat(
                        id = "later",
                        status = "succeeded",
                        run = done,
                        wakeup = wakeup(wakeAt = 9000),
                    )
                ),
                listOf(
                    Run(
                        id = "watch-1",
                        taskId = "77bb521a-0c2b-572d-b168-a7ad40ba98eb",
                        status = "waiting",
                        trigger = "manual",
                        wakeup = wakeup(wakeAt = 5000),
                    )
                ),
                listOf(Task("77bb521a-0c2b-572d-b168-a7ad40ba98eb", "Surveiller la CI")),
            )
        assertTrue(feed.running.isEmpty())
        assertEquals(listOf("run:watch-1", "chat:later"), feed.waiting.map { it.key })
        assertEquals(FeedKind.WAITING_TASK, feed.waiting[0].kind)
        assertEquals("Surveiller la CI", feed.waiting[0].title)
        assertEquals("Merger après la CI", feed.waiting[0].subtitle)
        assertEquals(5000L, feed.waiting[0].wakeAt)
        assertEquals("2 reprises prévues", feedSummary(feed))
    }

    // Grouped waits: several external results in one wakeup.
    private val runId = "11111111-2222-4333-8444-555555555555"
    private val runCondition = buildJsonObject {
        put("type", "run_finished")
        put("runId", runId)
    }
    private val failedRun = buildJsonObject {
        put("state", "completed")
        put("type", "run_finished")
        put("runId", runId)
        put("status", "failed")
    }
    private val passedChecks = buildJsonObject {
        put("state", "completed")
        put("type", "github_pr_checks")
        put("pluginId", "github")
        putJsonObject("display") {
            put("tone", "ok")
            put("title", "Required checks passed")
            put("detail", "")
        }
    }

    private fun group(mode: String) = buildJsonObject {
        put("type", "group")
        put("mode", mode)
        putJsonArray("conditions") {
            add(runCondition)
            add(checks)
        }
    }

    private fun groupWakeup(mode: String, vararg members: JsonObject) =
        wakeup(group(mode)).copy(display = null, members = JsonArray(members.toList()))

    private fun groupResult(state: String, mode: String, vararg results: JsonElement) =
        buildJsonObject {
            put("state", state)
            put("type", "group")
            put("mode", mode)
            putJsonArray("members") {
                results.forEach { result ->
                    addJsonObject {
                        put("condition", checks)
                        put("result", result)
                    }
                }
            }
        }

    @Test
    fun `a group is named by its size and how many results arrived`() {
        val pending =
            groupWakeup(
                "all",
                buildJsonObject { put("observation", failedRun) },
                buildJsonObject {
                    put("display", display)
                    put("nextCheckAt", 30_000)
                },
            )
        assertEquals(
            WaitTarget(WaitKind.GROUP, "Attente de 2 résultats", "1 sur 2 résultats", emptyList()),
            waitTarget(pending),
        )
        assertEquals(
            "2 résultats",
            waitTarget(groupWakeup("all", JsonObject(emptyMap()), JsonObject(emptyMap()))).short,
        )
        val any = waitTarget(groupWakeup("any", JsonObject(emptyMap()), JsonObject(emptyMap())))
        assertEquals("Attente du premier de 2 résultats", any.title)
        assertEquals("Premier de 2 résultats", any.short)
    }

    @Test
    fun `each grouped condition shows its own progress`() {
        val members =
            waitMembers(
                groupWakeup(
                    "all",
                    buildJsonObject { put("observation", failedRun) },
                    buildJsonObject {
                        put("display", display)
                        put("nextCheckAt", 30_000)
                        put("lastError", "Temporarily unavailable")
                    },
                )
            )
        assertEquals(
            listOf(
                WaitMember(
                    WaitKind.RUN,
                    "Exécution Cairn",
                    listOf("11111111"),
                    runId,
                    WaitOutcome(WaitTone.ERROR, "L’exécution a échoué", ""),
                ),
                WaitMember(
                    WaitKind.CONDITION,
                    "PR #193 checks",
                    listOf("0b2ddce"),
                    nextCheckAt = 30_000,
                    lastError = "Temporarily unavailable",
                ),
            ),
            members,
        )
        // Without its plugin's words, a condition reads as its type.
        assertEquals("Github pr checks", waitMembers(groupWakeup("all"))[1].title)
        // The final result names the conditions even without a wakeup.
        val result = groupResult("completed", "all", failedRun, passedChecks)
        assertEquals(
            listOf("L’exécution a échoué", "Required checks passed"),
            waitMembers(null, result).map { it.outcome?.title },
        )
        assertEquals("2 résultats", observedTarget(result))
    }

    @Test
    fun `a group sums up by its most serious result and never assumes missing ones`() {
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "Les 2 résultats sont arrivés", "1 en échec · 1 réussis"),
            waitOutcome(groupResult("completed", "all", failedRun, passedChecks)),
        )
        assertEquals(
            WaitOutcome(WaitTone.OK, "Les 2 résultats sont arrivés", ""),
            waitOutcome(groupResult("completed", "all", passedChecks, passedChecks)),
        )
        assertEquals(
            WaitOutcome(WaitTone.OK, "Premier résultat · Required checks passed", ""),
            waitOutcome(groupResult("completed", "any", JsonNull, passedChecks)),
        )
        assertEquals(
            WaitOutcome(WaitTone.WARNING, "Échéance atteinte", "1 sur 2 résultats"),
            waitOutcome(groupResult("timed_out", "all", passedChecks, JsonNull)),
        )
        assertEquals(
            WaitOutcome(WaitTone.WARNING, "Échéance atteinte", "Aucun résultat"),
            waitOutcome(groupResult("timed_out", "any", JsonNull, JsonNull)),
        )
    }

    @Test
    fun `delegated tasks retain their identity and failure outcome in a group`() {
        val first = buildJsonObject {
            put("type", "task_finished")
            put("chatId", "a11ce123-task")
        }
        val second = buildJsonObject {
            put("type", "task_finished")
            put("chatId", "b11ce123-task")
        }
        val failed = buildJsonObject {
            put("state", "completed")
            put("type", "task_finished")
            put("status", "failed")
            put("error", "Review failed")
        }
        val group = buildJsonObject {
            put("type", "group")
            put("mode", "all")
            put("conditions", JsonArray(listOf(first, second)))
        }
        val members =
            waitMembers(
                wakeup(group)
                    .copy(
                        members =
                            JsonArray(
                                listOf(
                                    buildJsonObject { put("observation", failed) },
                                    JsonObject(emptyMap()),
                                )
                            )
                    )
            )
        assertEquals(listOf("a11ce123-task", "b11ce123-task"), members.map { it.chatId })
        assertEquals(listOf("a11ce123"), members.first().details)
        assertEquals(WaitKind.TASK, members.first().kind)
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "La tâche a échoué", "Review failed"),
            members.first().outcome,
        )
        assertEquals(
            WaitTone.ERROR,
            waitOutcome(groupResult("completed", "all", failed, passedChecks)).tone,
        )
    }
}
