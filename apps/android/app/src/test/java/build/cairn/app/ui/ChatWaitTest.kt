package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ChatWaitTest {
    private val sha = "0b2ddce3a1b2c3d4e5f60718293a4b5c6d7e8f90"
    private val checks = buildJsonObject {
        put("type", "github_pr_checks")
        put("projectId", "project")
        put("prNumber", 193)
        put("headSha", sha)
    }

    private fun wakeup(condition: JsonObject? = null, wakeAt: Long = 1_800_000) =
        ChatWakeup("wake", "first", "Merger après la CI", 1800, 0, wakeAt, condition = condition)

    private var id = 0L

    private fun event(type: String, payload: JsonObject) =
        (++id).let { RunEvent(it, it * 60_000, type, "", payload) }

    private fun ChatWakeup.json() = wireJson.encodeToJsonElement(this).jsonObject

    private fun toolCall(tool: String, server: String, error: Boolean = false) =
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
                }
            },
        )

    @Test
    fun `names what the conversation waits for and counts down to it`() {
        assertEquals(
            WaitTarget(
                WaitKind.PR,
                "Attente des checks requis de la PR #193",
                "Checks PR #193",
                listOf("0b2ddce"),
            ),
            waitTarget(wakeup(checks)),
        )
        assertTrue(waitTarget(wakeup()).short.startsWith("Reprise à "))
        assertEquals("45 s", timeLeft(45_000))
        assertEquals("2 min", timeLeft(61_000))
        assertEquals("1 h 05", timeLeft(65 * 60_000L))
        assertEquals("dans 20 min", waitCountdown(wakeup(), 600_000))
        assertEquals("20 min max", waitCountdown(wakeup(checks), 600_000))
        assertEquals("Reprise imminente", waitCountdown(wakeup(), 2_000_000))
    }

    @Test
    fun `says how the wait ended`() {
        assertEquals(
            WaitOutcome(WaitTone.ERROR, "Checks requis en échec", "lint"),
            waitOutcome(
                buildJsonObject {
                    put("state", "completed")
                    put("type", "github_pr_checks")
                    put("conclusion", "failure")
                    putJsonArray("checks") {
                        addJsonObject {
                            put("name", "lint")
                            put("state", "failure")
                        }
                        addJsonObject {
                            put("name", "test")
                            put("state", "success")
                        }
                    }
                }
            ),
        )
        assertEquals(
            "La PR #193 a été fusionnée",
            waitOutcome(
                    buildJsonObject {
                        put("state", "closed")
                        put("prNumber", 193)
                        put("pullRequestState", "MERGED")
                    }
                )
                .title,
        )
        assertEquals(WaitTone.NEUTRAL, waitOutcome(null).tone)
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
                    ),
                ),
                chat = true,
            )
        assertEquals(listOf(false, true, false), entries.map { it.message })
        assertEquals("message", entries[0].wait?.ended)
        assertNull(entries[2].wait?.wakeup)
        assertTrue(entries[2].wait!!.finished)
    }

    @Test
    fun `lists waiting conversations apart, soonest first, unless paused`() {
        val done = Run("run", status = "succeeded")
        val feed =
            buildFeed(
                listOf(
                    Chat(
                        "later",
                        status = "succeeded",
                        run = done,
                        updatedAt = 3,
                        wakeup = wakeup(checks, 9000),
                    ),
                    Chat(
                        "sooner",
                        status = "succeeded",
                        run = done,
                        updatedAt = 2,
                        wakeup = wakeup(wakeAt = 5000),
                    ),
                    Chat(
                        "paused",
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
        assertEquals("Checks PR #193", feed.waiting[1].subtitle)
        assertTrue(feed.waiting[1].condition)
        assertEquals(listOf("chat:paused"), feed.recent.map { it.key })
        assertEquals(feed.keys(), listOf("chat:sooner", "chat:later", "chat:paused"))
    }
}
