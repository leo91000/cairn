package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ConversationPresentationTest {
    @Test
    fun `later admission waits do not borrow a recovered quota message or reset`() {
        val old = "You've hit your individual spend limit · resets Monday 3pm (UTC)"
        fun failure(id: Long, message: String) =
            RunEvent(
                id = id,
                createdAt = id,
                type = "turn.failed",
                text = "",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        buildJsonObject {
                            put("error", buildJsonObject { put("message", message) })
                        }
                    ),
                runId = "run",
            )
        val failed = listOf(failure(1, old))
        val recovered =
            failed +
                listOf(
                    RunEvent(
                        id = 2,
                        createdAt = 2,
                        type = "turn.started",
                        text = "",
                        runId = "run",
                    ),
                    RunEvent(
                        id = 3,
                        createdAt = 3,
                        type = "turn.completed",
                        text = "",
                        runId = "run",
                    ),
                    RunEvent(
                        id = 4,
                        createdAt = 4,
                        type = "status",
                        text = "succeeded",
                        runId = "run",
                    ),
                )
        val admission =
            Run(
                id = "run",
                status = "queued",
                accountWaitReason = "Waiting for a Codex account with available usage.",
                snapshot =
                    Snapshot(
                        agent =
                            Agent(
                                exclusiveNodes = emptyList(),
                                plugins = emptyList(),
                                id = "00000000-0000-4000-8000-000000000000",
                                name = "",
                                revision = "",
                                createdAt = 0,
                                access =
                                    AccessPolicy(
                                        mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                        projects = null,
                                        skills = null,
                                        mcps = null,
                                    ),
                                provider = "codex",
                            ),
                        task =
                            build.cairn.contracts.RunTaskSnapshot(
                                agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                id = "00000000-0000-4000-8000-000000000003",
                                name = "Fixture",
                                prompt = "Fixture",
                            ),
                    ),
                taskId = "",
            )
        val quota = chatQuota(admission, recovered)!!
        assertEquals("Codex", quota.provider)
        assertEquals("", quota.message)
        assertFalse(quota.spend)
        assertNull(quota.reset)
        assertTrue(quota.automatic)
        assertEquals("", chatQuota(admission, failed)!!.message)

        val waiting = admission.copy(status = "running", accountWaitReason = "Usage exhausted.")
        assertEquals("", chatQuota(waiting, recovered)!!.message)
        val current = "You've hit your usage limit · resets Tuesday 9am (UTC)"
        val interrupted = recovered + failure(5, current)
        assertEquals(current, chatQuota(waiting, interrupted)!!.message)
        assertEquals("Tuesday 9am (UTC)", chatQuota(waiting, interrupted)!!.reset)
        assertEquals(
            current,
            chatQuota(waiting.copy(status = "failed", error = current), failed)!!.message,
        )
    }

    @Test
    fun `quota history supports structured failures and retry notices`() {
        val text = "Account usage is exhausted."
        fun event(id: Long, type: String, payload: JsonObject? = null, raw: String = "") =
            RunEvent(
                id = id,
                createdAt = id,
                type = type,
                text = raw,
                payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
                runId = "run",
            )
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
            val waiting =
                Run(
                    id = "run",
                    status = "running",
                    accountWaitReason = "Usage exhausted.",
                    taskId = "",
                )
            assertEquals(text, chatQuota(waiting, history)!!.message)
            val retrying =
                history.dropLast(1) +
                    event(3, "error", JsonObject(failure + ("willRetry" to JsonPrimitive(true))))
            assertTrue(quotaHiddenEvents(retrying).isEmpty())
        }
    }

    @Test
    fun `a reply that only repeats the turn failure folds into its notice`() {
        val overloaded = "API Error: 529 Overloaded"
        fun event(id: Long, type: String, payload: JsonObject? = null, raw: String = "") =
            RunEvent(
                id = id,
                createdAt = id,
                type = type,
                text = raw,
                payload = (payload)?.let { kotlinx.serialization.json.JsonObject(it) },
                runId = "run",
            )
        fun reply(id: Long, text: String) =
            event(
                id,
                "item.completed",
                buildJsonObject {
                    put(
                        "item",
                        buildJsonObject {
                            put("id", "m$id")
                            put("type", "agent_message")
                            put("text", text)
                        },
                    )
                },
            )
        val error = buildJsonObject {
            put("error", buildJsonObject { put("message", overloaded) })
        }
        val history =
            listOf(
                event(1, "chat.user", raw = "Hello"),
                event(2, "turn.started"),
                reply(3, "Work is preserved."),
                reply(4, overloaded),
                event(5, "turn.failed", error),
                event(6, "status", error, raw = "failed"),
            )
        assertEquals(setOf(4L), quotaHiddenEvents(history))
    }

    @Test
    fun `quota waits stop the working label and saved failures require manual resume`() {
        val message = "You've hit your individual spend limit · your weekly limit resets 3pm (UTC)"
        val run =
            Run(
                id = "run",
                status = "running",
                accountWaitReason =
                    "Usage exhausted. Waiting for an available Claude Code account to resume.",
                taskId = "",
            )
        val chat = Chat(id = "chat", run = run)
        assertEquals(
            ConversationState("En attente de quota", null),
            conversationState(chat, "Agent", null),
        )
        assertTrue(chatQuota(run, emptyList())!!.automatic)
        val paused = chat.copy(paused = true)
        assertEquals("En pause", conversationState(paused, "Agent", null).status)
        assertTrue(chatQuota(paused.run, emptyList())!!.automatic)
        val saved = chatQuota(run.copy(status = "failed", error = message), emptyList())!!
        assertFalse(saved.automatic)
        assertEquals("3pm (UTC)", saved.reset)
        assertTrue(saved.spend)
        assertNull(
            chatQuota(run.copy(status = "failed", error = "Authentication failed"), emptyList())
        )
        assertNull(chatQuota(run.copy(status = "succeeded", accountWaitReason = null), emptyList()))
    }

    @Test
    fun `a failed run's banner is hidden once its reason shows as a failure notice`() {
        val capacity = "Selected model is at capacity. Please try a different model."
        val failure =
            RunEvent(
                id = 1,
                createdAt = 1,
                type = "turn.failed",
                text = capacity,
                payload =
                    kotlinx.serialization.json.JsonObject(
                        buildJsonObject {
                            put("error", buildJsonObject { put("message", capacity) })
                        }
                    ),
                runId = "run",
            )
        val chat =
            Chat(
                id = "chat",
                run =
                    Run(
                        id = "run",
                        status = "failed",
                        error = capacity,
                        taskId = "",
                    ),
            )
        assertTrue(failureShown(chat, listOf(failure)))
        assertFalse(failureShown(chat, emptyList()))
        assertFalse(failureShown(chat, listOf(failure.copy(runId = "other"))))
        assertFalse(failureShown(chat.copy(error = "Preparation failed"), listOf(failure)))
        assertFalse(
            failureShown(chat.copy(run = chat.run!!.copy(status = "queued")), listOf(failure))
        )
        // The timeline drops an adapter's report of a stop: it cannot stand for the reason.
        val stop = "Claude execution stopped."
        val echo =
            failure.copy(
                text = stop,
                payload =
                    (buildJsonObject { put("error", buildJsonObject { put("message", stop) }) })
                        ?.let { kotlinx.serialization.json.JsonObject(it) },
            )
        assertFalse(failureShown(chat.copy(run = chat.run!!.copy(error = stop)), listOf(echo)))
        // A run records at most 10,000 characters of its error.
        val truncated = chat.copy(run = chat.run!!.copy(error = capacity.take(20)))
        assertTrue(failureShown(truncated, listOf(failure)))
        // Quota bookkeeping hides the failed status of an exhausted turn: the banner keeps its
        // reason.
        val limit = "You've hit your usage limit"
        val timeLimit = "Run exceeded its time limit."
        val exhausted =
            listOf(
                RunEvent(
                    id = 1,
                    createdAt = 1,
                    type = "turn.started",
                    text = "",
                    runId = "run",
                ),
                RunEvent(
                    id = 2,
                    createdAt = 2,
                    type = "turn.failed",
                    text = limit,
                    payload =
                        kotlinx.serialization.json.JsonObject(
                            buildJsonObject {
                                put("error", buildJsonObject { put("message", limit) })
                            }
                        ),
                    runId = "run",
                ),
                RunEvent(
                    id = 3,
                    createdAt = 3,
                    type = "status",
                    text = "failed",
                    payload =
                        kotlinx.serialization.json.JsonObject(
                            buildJsonObject {
                                put("error", buildJsonObject { put("message", timeLimit) })
                            }
                        ),
                    runId = "run",
                ),
            )
        assertFalse(failureShown(chat.copy(run = chat.run!!.copy(error = timeLimit)), exhausted))
    }

    private val first = ChatMessage(id = "one", text = "Bonjour")
    private val second = ChatMessage(id = "two", text = "Ensuite")

    private val retryRun =
        Run(
            id = "retry",
            status = "queued",
            accountWaitReason = "Temporary agent error. Retrying in 30 seconds (attempt 1/5).",
            retry =
                RunRetry(
                    attempt = 1,
                    limit = 5,
                    nextAttemptAt = 31_000,
                    cause =
                        RunRetryCause(
                            kind = "timeout",
                            message = "Temporary timeout",
                        ),
                ),
            taskId = "",
        )

    @Test
    fun `retry counts down locally with ceiling and never shows negative seconds`() {
        assertEquals(retryRun.accountWaitReason, retryNotice(retryRun, 1000))
        assertEquals(
            "Temporary agent error. Retrying in 20 seconds (attempt 1/5).",
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
                """{"id":"retry","status":"queued","retry":{"attempt":2,"limit":5,"nextAttemptAt":31000,"cause":{"kind":"timeout","message":"Temporary timeout"}},"taskId":"fixture-task"}"""
            )
        assertEquals(31_000L, decoded.retry!!.nextAttemptAt)
        assertEquals("timeout", decoded.retry.cause.kind)
        assertTrue(retryNotice(decoded, 1000)!!.contains("attempt 2/5"))
        assertEquals(
            null,
            wireJson
                .decodeFromString<Run>("""{"id":"r","status":"queued","taskId":"fixture-task"}""")
                .retry,
        )
        assertEquals(
            null,
            wireJson
                .decodeFromString<Run>(
                    """{"id":"r","status":"queued","retry":null,"taskId":"fixture-task"}"""
                )
                .retry,
        )
        assertEquals(
            null,
            wireJson
                .decodeFromString<Run>(
                    """{"id":"r","status":"queued","retry":{"attempt":1,"limit":5,"nextAttemptAt":null,"cause":{"kind":"server","message":"Busy"}},"taskId":"fixture-task"}"""
                )
                .retry!!
                .nextAttemptAt,
        )
    }

    @Test
    fun `exclusive suspension stops working presentation without replacing failures or a user pause`() {
        val reservation = ExclusiveReservation("reservation", "node", "owner", "Build", "acquired")
        val running =
            Run(
                id = "run",
                status = "running",
                suspendedByExclusive = reservation,
                taskId = "",
            )
        val chat = Chat(id = "chat", status = "running", run = running)
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
                id = "chat",
                run =
                    Run(
                        id = "run",
                        status = "succeeded",
                        taskId = "",
                    ),
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
                id = "chat",
                run =
                    Run(
                        id = "run",
                        status = "succeeded",
                        taskId = "",
                    ),
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
        assertNull(
            scheduledWakeup(
                chat.copy(
                    run =
                        Run(
                            id = "run",
                            status = "running",
                            taskId = "",
                        )
                )
            )
        )
        assertNull(scheduledWakeup(chat.copy(wakeup = null)))
    }

    @Test
    fun `follow-ups stay queued behind a scheduled wait`() {
        val chat =
            Chat(
                id = "chat",
                run =
                    Run(
                        id = "run",
                        status = "succeeded",
                        taskId = "",
                    ),
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
                id = "run",
                status = "queued",
                chatExecution = ChatExecution(messageId = "one"),
                accountWaitReason =
                    "Reconnect your Claude Code account in Connections to continue.",
                accountRequired = "claude",
                taskId = "",
            )
        val chat = Chat(id = "chat", run = run, messages = listOf(first))
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
                id = "run",
                status = "queued",
                accountWaitReason = reason,
                chatExecution = ChatExecution(messageId = "one"),
                taskId = "",
            )
        assertEquals(ChatWaitNotice(reason), chatWaitNotice(run))
        assertEquals(
            "En attente de l’agent…",
            chatDelivery(Chat(id = "chat", run = run, messages = listOf(first)), emptyList())
                .sending
                .single()
                .label,
        )
        assertNull(chatWaitNotice(run.copy(accountWaitReason = "")))
        assertNull(chatWaitNotice(run.copy(status = "succeeded")))
    }

    @Test
    fun `idle send is in the transcript but later followups wait`() {
        val delivery =
            chatDelivery(Chat(id = "chat", messages = listOf(first, second)), emptyList())
        assertEquals(listOf("one"), delivery.sending.map { it.message.id })
        assertEquals(listOf("two"), delivery.queued.map { it.id })
    }

    @Test
    fun `dispatch of the first message does not turn it into a queued followup`() {
        val run =
            Run(
                id = "run",
                status = "running",
                chatExecution = ChatExecution(messageId = "one"),
                taskId = "",
            )
        val delivery =
            chatDelivery(
                Chat(id = "chat", run = run, messages = listOf(first, second)),
                emptyList(),
            )
        assertEquals("Démarrage de l’agent…", delivery.sending.single().label)
        assertEquals("two", delivery.queued.single().id)
    }

    @Test
    fun `paused and failed runs keep messages waiting`() {
        listOf(
                Chat(id = "chat", paused = true),
                Chat(
                    id = "chat",
                    run =
                        Run(
                            id = "r",
                            status = "failed",
                            taskId = "",
                        ),
                ),
            )
            .forEach {
                val result = chatDelivery(it.copy(messages = listOf(first)), emptyList())
                assertTrue(result.sending.isEmpty())
                assertEquals(first, result.queued.single())
            }
    }

    @Test
    fun `steering and acknowledgements do not duplicate or expose private replies`() {
        val run =
            Run(
                id = "run",
                status = "running",
                taskId = "",
            )
        val privateAnswer =
            first.copy(id = "private", questionId = "question", mode = "steer", text = "secret")
        val chat =
            Chat(
                id = "chat",
                run = run,
                messages = listOf(first.copy(mode = "steer"), privateAnswer),
            )
        assertEquals(listOf("one"), chatDelivery(chat, emptyList()).sending.map { it.message.id })
        val ack =
            RunEvent(
                id = 1,
                createdAt = 1,
                type = "chat.user",
                text = "Bonjour",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        mapOf("messageId" to JsonPrimitive("one"))
                    ),
            )
        assertTrue(chatDelivery(chat, listOf(ack), first).sending.isEmpty())
    }

    @Test
    fun `local optimistic send and saved message share one identity`() {
        val chat = Chat(id = "chat", messages = listOf(first))
        assertEquals(1, chatDelivery(chat, emptyList(), first).sending.size)
    }

    @Test
    fun `run outcomes and project revisions survive decoding and task attention grouping`() {
        val run =
            wireJson.decodeFromString<Run>(
                """{"id":"r","status":"succeeded","outcome":{"status":"blocked","reason":"Missing access","evidence":["403"],"reportedAt":10},"workspaces":[{"projectId":"p","path":"/p","revision":"abcdef","kind":"worktree"}],"taskId":"fixture-task"}"""
            )
        assertEquals("À examiner", taskGroup(Task(), run))
        assertEquals("abcdef", run.workspaces.single().revision)
        assertEquals(listOf("403"), run.outcome!!.evidence)
        val project =
            wireJson.decodeFromString<Project>(
                """{"id":"00000000-0000-4000-8000-000000000001","name":"Project","path":"/project","origin":"","revision":"fixture","createdAt":0,"sourceMode":"local"}"""
            )
        assertEquals("local", project.sourceMode)
        assertEquals(
            "Terminées",
            taskGroup(Task(), run.copy(outcome = run.outcome.copy(status = "completed"))),
        )
    }

    @Test
    fun `the header names a background wait, a failure or an interruption`() {
        // The subtitle names the agent and its installation; the project moved to the sheet.
        val run =
            Run(
                id = "run",
                status = "running",
                startedAt = 1,
                taskId = "",
            )
        val chat = Chat(id = "chat", agentName = "Cairn", projectName = "Site", run = run)
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
        val chat = Chat(id = "chat", agentName = "Cairn", projectName = "Site")
        assertEquals(ConversationState("Cairn", null), conversationState(chat, "Cairn", null, null))
        assertEquals(
            "Échec · Cairn",
            conversationState(
                    chat.copy(
                        run =
                            Run(
                                id = "run",
                                status = "failed",
                                taskId = "",
                            )
                    ),
                    "Cairn",
                    null,
                )
                .status,
        )
        assertEquals(
            ConversationState("Cairn", null),
            conversationState(null, "Cairn", null, null, newChat = true),
        )
    }

    @Test
    fun `a failed run explains why the response stopped`() {
        val run =
            Run(
                id = "run",
                status = "failed",
                taskId = "",
            )
        assertEquals(
            "La réponse s’est arrêtée avant la fin. Reprenez la conversation pour continuer.",
            conversationError(Chat(id = "chat", run = run)),
        )
        assertEquals(
            "Usage limit reached",
            conversationError(Chat(id = "chat", run = run.copy(error = "Usage limit reached"))),
        )
        // The conversation's own error comes first.
        assertEquals(
            "Upload failed",
            conversationError(
                Chat(
                    id = "chat",
                    run = run.copy(error = "Usage limit reached"),
                    error = "Upload failed",
                )
            ),
        )
        assertNull(conversationError(Chat(id = "chat", run = run.copy(status = "succeeded"))))
        assertNull(
            conversationError(Chat(id = "chat", run = run.copy(error = " ", status = "running")))
        )
        assertNull(conversationError(null))
    }
}
