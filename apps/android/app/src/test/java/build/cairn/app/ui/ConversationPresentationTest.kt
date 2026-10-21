package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ConversationPresentationTest {
    @Test
    fun `quota history supports structured failures retry notices and plain legacy replies`() {
        val text = "Account usage is exhausted."
        fun event(id: Long, type: String, payload: JsonObject? = null, raw: String = "") =
            RunEvent(id, id, type, raw, payload, runId = "run")
        val reply =
            event(
                2,
                "item.completed",
                buildJsonObject {
                    put(
                        "item",
                        buildJsonObject {
                            put("id", "echo")
                            put("type", "agent_message")
                            put("text", text)
                        },
                    )
                },
            )
        for ((key, value) in
            listOf("code" to "usage_limit_reached", "codexErrorInfo" to "usageLimitExceeded")) {
            val failure = buildJsonObject {
                put(key, value)
                put("message", text)
            }
            val history = listOf(event(1, "turn.started"), reply, event(3, "error", failure))
            assertEquals(setOf(2L, 3L), quotaHiddenEvents(history))
            val waiting = Run("run", status = "running", accountWaitReason = "Usage exhausted.")
            assertEquals(text, chatQuota(waiting, history)!!.message)
            val retrying =
                history.dropLast(1) +
                    event(3, "error", JsonObject(failure + ("willRetry" to JsonPrimitive(true))))
            assertTrue(quotaHiddenEvents(retrying).isEmpty())
        }
        val message = "You've hit your individual spend limit"
        val legacy =
            listOf(
                event(1, "turn.started"),
                event(2, "item.started", raw = "cat log"),
                event(3, "item.completed", raw = message),
                event(4, "item.completed", raw = message),
                event(
                    5,
                    "turn.failed",
                    raw = """{"type":"turn.failed","error":{"message":"$message"}}""",
                ),
            )
        assertEquals(setOf(4L, 5L), quotaHiddenEvents(legacy))
        val entries = quotaTimeline(timelineEntries(legacy, chat = true), legacy)
        assertEquals(1, entries.size)
        assertFalse(entries.single().message)
    }

    @Test
    fun `quota waits stop the working label and saved failures require manual resume`() {
        val message = "You've hit your individual spend limit · your weekly limit resets 3pm (UTC)"
        val run =
            Run(
                "run",
                status = "running",
                accountWaitReason =
                    "Usage exhausted. Waiting for an available Claude Code account to resume.",
            )
        val chat = Chat("chat", run = run)
        assertEquals(
            ConversationState("En attente de quota", null),
            conversationState(chat, "Agent", null),
        )
        assertTrue(chatQuota(run, emptyList())!!.automatic)
        assertFalse(chatQuota(run, emptyList(), paused = true)!!.automatic)
        val saved = chatQuota(run.copy(status = "failed", error = message), emptyList())!!
        assertFalse(saved.automatic)
        assertEquals("3pm (UTC)", saved.reset)
        assertTrue(saved.spend)
        assertNull(
            chatQuota(run.copy(status = "failed", error = "Authentication failed"), emptyList())
        )
        assertNull(chatQuota(run.copy(status = "succeeded", accountWaitReason = null), emptyList()))
    }

    private val first = ChatMessage("one", text = "Bonjour")
    private val second = ChatMessage("two", text = "Ensuite")

    private val retryRun =
        Run(
            "retry",
            status = "queued",
            accountWaitReason = "Temporary agent error. Retrying in 30 seconds (attempt 1/3).",
            retry = RunRetry(1, 3, 31_000, RunRetryCause("timeout", "Temporary timeout")),
        )

    @Test
    fun `retry counts down locally with ceiling and never shows negative seconds`() {
        assertEquals(retryRun.accountWaitReason, retryNotice(retryRun, 1000))
        assertEquals(
            "Temporary agent error. Retrying in 20 seconds (attempt 1/3).",
            chatWaitNotice(retryRun, 11_100)!!.message,
        )
        assertTrue(retryNotice(retryRun, 30_999)!!.contains("in 1 seconds"))
        assertTrue(retryNotice(retryRun, 31_000)!!.contains("Retrying now"))
        assertTrue(retryNotice(retryRun, 40_000)!!.contains("Retrying now"))
        assertNotNull(retryNotice(retryRun.copy(accountWaitReason = null), 1000))
        assertNotNull(retryNotice(retryRun.copy(accountWaitReason = "  "), 1000))
    }

    @Test
    fun `retry does not hide account capacity or fencing waits and clears on start or cancellation`() {
        assertNull(retryNotice(retryRun.copy(accountRequired = "codex"), 1000))
        listOf("running", "succeeded", "failed", "cancelled", "interrupted").forEach {
            assertNull(retryNotice(retryRun.copy(status = it), 1000))
            assertNull(chatWaitNotice(retryRun.copy(status = it), 1000))
        }
        listOf("Waiting for capacity.", "Waiting for the previous VM to stop.").forEach {
            val run = retryRun.copy(accountWaitReason = it)
            assertNull(retryNotice(run, 1000))
            assertEquals(ChatWaitNotice(it), chatWaitNotice(run, 1000))
        }
        assertEquals(
            "Codex",
            chatWaitNotice(retryRun.copy(accountRequired = "codex"), 1000)!!.account,
        )
    }

    @Test
    fun `missing retry deadline retains the server reason`() {
        listOf<RunRetry?>(null, retryRun.retry!!.copy(nextAttemptAt = null)).forEach {
            val run = retryRun.copy(retry = it)
            assertNull(retryNotice(run, 1000))
            assertEquals(ChatWaitNotice(run.accountWaitReason!!), chatWaitNotice(run, 1000))
        }
    }

    @Test
    fun `retry shared contract decodes nullable and absent records and deadlines`() {
        val decoded =
            wireJson.decodeFromString<Run>(
                """{"id":"retry","status":"queued","retry":{"attempt":2,"limit":3,"nextAttemptAt":31000,"cause":{"kind":"timeout","message":"Temporary timeout"}}}"""
            )
        assertEquals(31_000L, decoded.retry!!.nextAttemptAt)
        assertEquals("timeout", decoded.retry.cause.kind)
        assertTrue(retryNotice(decoded, 1000)!!.contains("attempt 2/3"))
        assertEquals(null, wireJson.decodeFromString<Run>("""{"id":"r","status":"queued"}""").retry)
        assertEquals(
            null,
            wireJson.decodeFromString<Run>("""{"id":"r","status":"queued","retry":null}""").retry,
        )
        assertEquals(
            null,
            wireJson
                .decodeFromString<Run>(
                    """{"id":"r","status":"queued","retry":{"attempt":1,"limit":3,"nextAttemptAt":null,"cause":{"kind":"server","message":"Busy"}}}"""
                )
                .retry!!
                .nextAttemptAt,
        )
    }

    @Test
    fun `exclusive suspension stops working presentation without replacing failures or a user pause`() {
        val reservation = ExclusiveReservation("reservation", "node", "owner", "Build", "acquired")
        val running = Run("run", status = "running", suspendedByExclusive = reservation)
        val chat = Chat("chat", status = "running", run = running)
        assertEquals(
            ConversationState("Suspendue (exclusivité)", null),
            conversationState(chat, "Cairn", BackgroundWait(listOf("Build"), 1), "Maison"),
        )
        assertEquals(
            ConversationState("Suspendue (exclusivité)", null),
            conversationState(
                chat.copy(run = null, suspendedByExclusive = reservation),
                "Cairn",
                null,
            ),
        )
        assertEquals("En pause", conversationState(chat.copy(paused = true), "Cairn", null).status)
        assertEquals(
            ConversationState("Échec · Cairn · Maison", null),
            conversationState(
                chat.copy(run = running.copy(status = "failed")),
                "Cairn",
                null,
                "Maison",
            ),
        )
        assertEquals("failed", runPresentationStatus(running.copy(status = "failed")))
    }

    @Test
    fun `condition wait distinguishes a result from its fallback deadline`() {
        val condition = buildJsonObject {
            put("type", "run_finished")
            put("runId", "other-run")
        }
        val chat =
            Chat(
                "chat",
                run = Run("run", status = "succeeded"),
                wakeup =
                    ChatWakeup(
                        "wake",
                        "one",
                        "Attendre la CI",
                        420,
                        1,
                        420001,
                        condition = condition,
                    ),
            )
        assertEquals(chat.wakeup, scheduledWakeup(chat))
        assertEquals(
            "Résultat attendu · 7 min max",
            conversationState(chat, "Agent", null, now = 1).status,
        )
        assertTrue(chatDelivery(chat, emptyList()).sending.isEmpty())
        assertNull(scheduledWakeup(chat.copy(paused = true)))
    }

    @Test
    fun `scheduled wait names the wakeup without inventing a user message`() {
        val chat =
            Chat(
                "chat",
                run = Run("run", status = "succeeded"),
                wakeup = ChatWakeup("wake", "one", "Attendre la CI", 420, 1, 420001),
            )
        assertEquals(chat.wakeup, scheduledWakeup(chat))
        assertEquals("Reprise dans 7 min", conversationState(chat, "Agent", null, now = 1).status)
        assertEquals(
            "Reprise imminente",
            conversationState(chat, "Agent", null, now = 420001).status,
        )
        assertTrue(chatDelivery(chat, emptyList()).sending.isEmpty())
        assertNull(scheduledWakeup(chat.copy(paused = true)))
        assertNull(scheduledWakeup(chat.copy(lifecycle = "trash")))
        assertNull(scheduledWakeup(chat.copy(run = Run("run", status = "running"))))
        assertNull(scheduledWakeup(chat.copy(wakeup = null)))
    }

    @Test
    fun `follow-ups stay queued behind a scheduled wait`() {
        val chat =
            Chat(
                "chat",
                run = Run("run", status = "succeeded"),
                messages = listOf(second),
                wakeup = ChatWakeup("wake", "one", "Attendre la CI", 420, 1, 420001),
            )
        assertEquals(ChatDelivery(emptyList(), listOf(second)), chatDelivery(chat, emptyList()))
        assertEquals(
            second,
            chatDelivery(chat.copy(wakeup = null), emptyList()).sending.single().message,
        )
    }

    @Test
    fun `queued Claude message explains the missing account and clears notice when running`() {
        val run =
            Run(
                "run",
                status = "queued",
                chatExecution = ChatExecution("one"),
                accountWaitReason =
                    "Reconnect your Claude Code account in Connections to continue.",
                accountRequired = "claude",
            )
        val chat = Chat("chat", run = run, messages = listOf(first))
        assertEquals("Claude Code", chatWaitNotice(run)!!.account)
        assertEquals(
            "En attente d’un compte Claude Code",
            chatDelivery(chat, emptyList()).sending.single().label,
        )
        assertNull(chatWaitNotice(run.copy(status = "running")))
        assertEquals(
            "Démarrage de l’agent…",
            chatDelivery(chat.copy(run = run.copy(status = "running")), emptyList())
                .sending
                .single()
                .label,
        )
    }

    @Test
    fun `other waiting reasons stay visible without a reconnect action`() {
        val reason = "Waiting for the previous execution to stop before recovery."
        val run =
            Run(
                "run",
                status = "queued",
                accountWaitReason = reason,
                chatExecution = ChatExecution("one"),
            )
        assertEquals(ChatWaitNotice(reason), chatWaitNotice(run))
        assertEquals(
            "En attente de l’agent…",
            chatDelivery(Chat("chat", run = run, messages = listOf(first)), emptyList())
                .sending
                .single()
                .label,
        )
        assertNull(chatWaitNotice(run.copy(accountWaitReason = "")))
        assertNull(chatWaitNotice(run.copy(status = "succeeded")))
    }

    @Test
    fun `idle send is in the transcript but later followups wait`() {
        val delivery = chatDelivery(Chat("chat", messages = listOf(first, second)), emptyList())
        assertEquals(listOf("one"), delivery.sending.map { it.message.id })
        assertEquals(listOf("two"), delivery.queued.map { it.id })
    }

    @Test
    fun `dispatch of the first message does not turn it into a queued followup`() {
        val run = Run("run", status = "running", chatExecution = ChatExecution("one"))
        val delivery =
            chatDelivery(Chat("chat", run = run, messages = listOf(first, second)), emptyList())
        assertEquals("Démarrage de l’agent…", delivery.sending.single().label)
        assertEquals("two", delivery.queued.single().id)
    }

    @Test
    fun `paused and failed runs keep messages waiting`() {
        listOf(Chat("chat", paused = true), Chat("chat", run = Run("r", status = "failed")))
            .forEach {
                val result = chatDelivery(it.copy(messages = listOf(first)), emptyList())
                assertTrue(result.sending.isEmpty())
                assertEquals(first, result.queued.single())
            }
    }

    @Test
    fun `steering and acknowledgements do not duplicate or expose private replies`() {
        val run = Run("run", status = "running")
        val privateAnswer =
            first.copy(id = "private", questionId = "question", mode = "steer", text = "secret")
        val chat =
            Chat("chat", run = run, messages = listOf(first.copy(mode = "steer"), privateAnswer))
        assertEquals(listOf("one"), chatDelivery(chat, emptyList()).sending.map { it.message.id })
        val ack = RunEvent(1, 1, "chat.user", "Bonjour", mapOf("messageId" to JsonPrimitive("one")))
        assertTrue(chatDelivery(chat, listOf(ack), first).sending.isEmpty())
    }

    @Test
    fun `local optimistic send and saved message share one identity`() {
        val chat = Chat("chat", messages = listOf(first))
        assertEquals(1, chatDelivery(chat, emptyList(), first).sending.size)
    }

    @Test
    fun `run outcomes and project revisions survive decoding and task attention grouping`() {
        val run =
            wireJson.decodeFromString<Run>(
                """{"id":"r","status":"succeeded","outcome":{"status":"blocked","reason":"Missing access","evidence":["403"],"reportedAt":10},"workspaces":[{"projectId":"p","path":"/p","revision":"abcdef"}]}"""
            )
        assertEquals("À examiner", taskGroup(Task(), run))
        assertEquals("abcdef", run.workspaces.single().revision)
        assertEquals(listOf("403"), run.outcome!!.evidence)
        val project = wireJson.decodeFromString<Project>("""{"sourceMode":"local"}""")
        assertEquals("local", project.sourceMode)
        assertEquals(
            "Terminées",
            taskGroup(Task(), run.copy(outcome = run.outcome.copy(status = "completed"))),
        )
    }

    @Test
    fun `the header names a background wait, a failure or an interruption`() {
        // The subtitle names the agent and its installation; the project moved to the sheet.
        val run = Run("run", status = "running", startedAt = 1)
        val chat = Chat("chat", agentName = "Cairn", projectName = "Site", run = run)
        assertEquals(
            ConversationState("Cairn · Maison", "Cairn travaille"),
            conversationState(chat, "Cairn", null, "Maison"),
        )
        assertEquals(
            ConversationState("Cairn · Maison", "Tâche en arrière-plan"),
            conversationState(chat, "Cairn", BackgroundWait(listOf("Build"), 1), "Maison"),
        )
        assertEquals(
            "2 tâches en arrière-plan",
            conversationState(chat, "Cairn", BackgroundWait(listOf("Build", "Lint"), 1), "Maison")
                .live,
        )
        // A wait announced earlier does not outlive the run.
        val failed = chat.copy(run = run.copy(status = "failed"))
        assertEquals(
            ConversationState("Échec · Cairn · Maison", null),
            conversationState(failed, "Cairn", BackgroundWait(listOf("Build"), 1), "Maison"),
        )
        assertEquals(
            "Interrompue · Cairn · Maison",
            conversationState(
                    chat.copy(run = run.copy(status = "interrupted")),
                    "Cairn",
                    null,
                    "Maison",
                )
                .status,
        )
        assertEquals(
            "En pause",
            conversationState(chat.copy(paused = true), "Cairn", null, "Maison").status,
        )
        assertEquals(
            "En attente",
            conversationState(chat.copy(run = run.copy(status = "queued")), "Cairn", null, "Maison")
                .status,
        )
        // A loading conversation stays quiet; a new one already names its agent and installation.
        assertEquals(ConversationState("", null), conversationState(null, "Cairn", null, "Maison"))
        assertEquals(
            ConversationState("Cairn · Maison", null),
            conversationState(null, "Cairn", null, "Maison", newChat = true),
        )
    }

    @Test
    fun `the header names only the agent while no installation is known`() {
        val chat = Chat("chat", agentName = "Cairn", projectName = "Site")
        assertEquals(ConversationState("Cairn", null), conversationState(chat, "Cairn", null, null))
        assertEquals(
            "Échec · Cairn",
            conversationState(chat.copy(run = Run("run", status = "failed")), "Cairn", null).status,
        )
        assertEquals(
            ConversationState("Cairn", null),
            conversationState(null, "Cairn", null, null, newChat = true),
        )
    }

    @Test
    fun `a failed run explains why the response stopped`() {
        val run = Run("run", status = "failed")
        assertEquals(
            "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer.",
            conversationError(Chat("chat", run = run)),
        )
        assertEquals(
            "Usage limit reached",
            conversationError(Chat("chat", run = run.copy(error = "Usage limit reached"))),
        )
        // The conversation's own error comes first.
        assertEquals(
            "Upload failed",
            conversationError(
                Chat("chat", run = run.copy(error = "Usage limit reached"), error = "Upload failed")
            ),
        )
        assertNull(conversationError(Chat("chat", run = run.copy(status = "succeeded"))))
        assertNull(conversationError(Chat("chat", run = run.copy(error = " ", status = "running"))))
        assertNull(conversationError(null))
    }
}
