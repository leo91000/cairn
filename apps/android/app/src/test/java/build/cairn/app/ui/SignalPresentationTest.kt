package build.cairn.app.ui

import build.cairn.app.data.*
import java.time.LocalDateTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SignalPresentationTest {
    @Test
    fun `exclusive suspension keeps active work visible without working animation and preserves attention`() {
        val reservation = ExclusiveReservation("reservation", "node", "owner", "Build", "acquired")
        val chat =
            Chat(
                "suspended",
                status = "running",
                agentName = "Cairn",
                suspendedByExclusive = reservation,
            )
        val mission =
            Run(
                id = "mission",
                taskId = "task",
                status = "running",
                suspendedByExclusive = reservation,
            )
        val feed =
            buildFeed(
                listOf(
                    chat,
                    chat.copy(id = "question", pendingQuestions = 1),
                    chat.copy(id = "failed", status = "failed", error = "Erreur prioritaire"),
                ),
                listOf(mission),
                emptyList(),
            )
        assertEquals(2, feed.running.size)
        assertTrue(
            feed.running.all { !it.working && it.subtitle.startsWith("Suspendue (exclusivité)") }
        )
        assertEquals(listOf(FeedKind.QUESTION, FeedKind.FAILED_CHAT), feed.forYou.map { it.kind })
        assertEquals("Erreur prioritaire", feed.forYou.last().subtitle)
    }

    @Test
    fun `automatic quota waits follow dated resumptions and preserve attention and suspension`() {
        val quota =
            Chat(
                "quota",
                status = "running",
                accountWaitReason =
                    "Usage exhausted. Waiting for an available Codex account to resume.",
            )
        val feed =
            buildFeed(
                listOf(
                    quota,
                    quota.copy(
                        id = "admission",
                        status = "queued",
                        accountWaitReason =
                            "Waiting for a Claude Code account with available usage.",
                    ),
                    quota.copy(id = "question", pendingQuestions = 1),
                    quota.copy(id = "paused", paused = true),
                    quota.copy(id = "failed", status = "failed"),
                    quota.copy(id = "reconnect", accountRequired = "codex"),
                    quota.copy(
                        id = "suspended",
                        suspendedByExclusive =
                            ExclusiveReservation(
                                "reservation",
                                "node",
                                "owner",
                                "Build",
                                "acquired",
                            ),
                    ),
                    Chat(
                        "dated",
                        status = "succeeded",
                        wakeup =
                            ChatWakeup(
                                id = "wake",
                                reason = "",
                                seconds = 60,
                                createdAt = 1,
                                wakeAt = 300,
                            ),
                    ),
                ),
                listOf(
                    Run(
                        id = "mission-run",
                        taskId = "mission",
                        status = "queued",
                        accountWaitReason = quota.accountWaitReason,
                    )
                ),
                emptyList(),
            )
        assertEquals(
            listOf("chat:dated", "chat:quota", "chat:admission", "run:mission-run"),
            feed.waiting.map { it.key },
        )
        assertTrue(
            feed.waiting.drop(1).all {
                it.subtitle == "En attente de quota" && it.wakeAt == null && !it.working
            }
        )
        assertEquals(listOf("chat:question", "chat:failed"), feed.forYou.map { it.key })
        assertEquals(listOf("chat:paused"), feed.recent.map { it.key })
        assertEquals(listOf("chat:reconnect", "chat:suspended"), feed.running.map { it.key })
        assertEquals(
            FeedKind.RUNNING_CHAT,
            buildFeed(listOf(quota.copy(accountWaitReason = null)), emptyList(), emptyList())
                .running
                .single()
                .kind,
        )
    }

    private val zone = ZoneId.of("Europe/Paris")

    private fun at(year: Int, month: Int, day: Int, hour: Int, minute: Int = 0) =
        LocalDateTime.of(year, month, day, hour, minute).atZone(zone).toInstant().toEpochMilli()

    @Test
    fun `feed puts questions, failures and reconnections first, then live work, then the rest`() {
        val chats =
            listOf(
                Chat(
                    "question",
                    "Choisir la pagination",
                    agentName = "Designer",
                    pendingQuestions = 2,
                    updatedAt = 5,
                ),
                Chat(
                    "running",
                    "Refonte Android",
                    agentName = "Cairn",
                    projectName = "Manager",
                    status = "running",
                    run =
                        Run(
                            id = "r1",
                            status = "running",
                            startedAt = 100,
                            taskId = "",
                        ),
                    updatedAt = 4,
                ),
                Chat("queued", "En file", agentName = "Cairn", status = "queued", updatedAt = 3),
                Chat(
                    "paused",
                    "Mise en pause",
                    agentName = "Cairn",
                    status = "running",
                    paused = true,
                    updatedAt = 2,
                ),
                Chat(
                    "failed",
                    "Échec du build",
                    agentName = "Ops",
                    status = "failed",
                    error = "Tests rouges\ndétail",
                    updatedAt = 1,
                ),
                Chat(
                    "reconnect",
                    "Attente Claude",
                    agentName = "Claude",
                    status = "queued",
                    updatedAt = 6,
                    run =
                        Run(
                            id = "r2",
                            status = "queued",
                            accountWaitReason =
                                "Connect a Claude Code account in Connections before running this agent.",
                            accountRequired = "claude",
                            taskId = "",
                        ),
                ),
                Chat("idle", "Ancienne conversation", agentName = "Cairn", updatedAt = 0),
            )
        val tasks =
            listOf(
                Task(
                    "a05f5991-ec1e-57a2-a8e5-ec314581a110",
                    "Revue quotidienne",
                    agentId = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
                ),
                Task("456fb838-d909-52a8-8fd7-8e95f2792cad", "Archivée", archived = true),
            )
        val activity =
            listOf(
                Run(
                    id = "t1",
                    taskId = "a05f5991-ec1e-57a2-a8e5-ec314581a110",
                    status = "failed",
                    trigger = "manual",
                    finishedAt = 50,
                    snapshot =
                        Snapshot(
                            agent =
                                Agent(
                                    exclusiveNodes = emptyList(),
                                    plugins = emptyList(),
                                    revision = "",
                                    createdAt = 0,
                                    access =
                                        AccessPolicy(
                                            mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                            projects = null,
                                            skills = null,
                                            mcps = null,
                                        ),
                                    id = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
                                    name = "Reviewer",
                                ),
                            task =
                                build.cairn.contracts.RunTaskSnapshot(
                                    agentId = build.cairn.app.data.MAIN_AGENT_ID,
                                    id = "00000000-0000-4000-8000-000000000003",
                                    name = "Fixture",
                                    prompt = "Fixture",
                                ),
                        ),
                ),
                Run(
                    id = "t2",
                    taskId = "456fb838-d909-52a8-8fd7-8e95f2792cad",
                    status = "failed",
                    trigger = "manual",
                ),
                Run(
                    id = "t3",
                    taskId = "chat-task",
                    status = "failed",
                    trigger = "chat",
                ),
            )
        val feed = buildFeed(chats, activity, tasks)
        assertEquals(
            listOf(
                FeedKind.RECONNECT,
                FeedKind.QUESTION,
                FeedKind.FAILED_CHAT,
                FeedKind.FAILED_TASK,
            ),
            feed.forYou.map { it.kind },
        )
        assertEquals("2 questions vous attendent", feed.forYou[1].subtitle)
        assertEquals("Tests rouges", feed.forYou[2].subtitle)
        val failedTask = feed.forYou.last()
        assertEquals("Revue quotidienne", failedTask.title)
        assertEquals("a05f5991-ec1e-57a2-a8e5-ec314581a110", failedTask.taskId)
        assertEquals("Reviewer", failedTask.agent)
        // Archived missions and chat runs never surface as missions.
        assertTrue(feed.forYou.none { it.runId == "t2" || it.runId == "t3" })
        assertEquals(listOf("chat:running", "chat:queued"), feed.running.map { it.key })
        assertEquals("Cairn · Manager", feed.running.first().subtitle)
        assertEquals("En attente · Cairn", feed.running.last().subtitle)
        // Only work that is running animates; queued work keeps its static badge.
        assertEquals(listOf(true, false), feed.running.map { it.working })
        assertEquals(listOf("chat:paused", "chat:idle"), feed.recent.map { it.key })
        assertTrue(feed.recent.first().subtitle.startsWith("En pause"))
    }

    @Test
    fun `each row carries the coding agent of its latest turn`() {
        val feed =
            buildFeed(
                listOf(
                    Chat(
                        "switched",
                        status = "running",
                        provider = "claude",
                        requester = ChatRequester("parent", "Review request"),
                    ),
                    Chat("new"),
                ),
                listOf(
                    Run(
                        id = "audit-run",
                        taskId = "b7ec3b50-62dd-530b-8060-bd5700fb7eb6",
                        status = "running",
                        provider = "opencode",
                    )
                ),
                listOf(Task("b7ec3b50-62dd-530b-8060-bd5700fb7eb6", "Audit")),
            )
        assertEquals(
            listOf("chat:switched" to "claude", "run:audit-run" to "opencode"),
            feed.running.map { it.key to it.harness },
        )
        assertNull(feed.recent.single().harness)
        assertEquals(
            FeedRequester("Review request", "parent", null),
            feed.running.single { it.chatId == "switched" }.requester,
        )
    }

    @Test
    fun `missions that need review and running missions are surfaced`() {
        val tasks =
            listOf(
                Task("8a025d72-4937-53c0-a41d-820414dc655d", "Audit"),
                Task("00eada4d-84f3-57cb-837e-3c023a643647", "Veille"),
            )
        val activity =
            listOf(
                Run(
                    id = "r1",
                    taskId = "8a025d72-4937-53c0-a41d-820414dc655d",
                    status = "succeeded",
                    outcome = TaskOutcome("blocked", "Accès manquant"),
                ),
                Run(
                    id = "r2",
                    taskId = "00eada4d-84f3-57cb-837e-3c023a643647",
                    status = "running",
                    startedAt = 10,
                ),
            )
        val feed = buildFeed(emptyList(), activity, tasks)
        assertEquals(FeedKind.REVIEW_TASK, feed.forYou.single().kind)
        assertEquals("Tâche bloquée", feed.forYou.single().subtitle)
        assertTrue(feed.running.single().working)
        assertEquals(FeedKind.RUNNING_TASK, feed.running.single().kind)
        assertEquals(10L, feed.running.single().startedAt)
    }

    @Test
    fun `blocked missions ask only when they wait on the user`() {
        val tasks =
            listOf(
                Task("2db836ee-ce2e-552d-b9fa-2fef2030546c", "Preuve appareil"),
                Task("3c7c4abf-37d8-5123-814f-356a0797b145", "Checks PR315"),
                Task("888f7003-e15b-5b1f-a1b3-a221bc92ec3a", "Déploiement staging"),
                Task("0a0e9fbd-72fd-586d-b159-15ebbd5e1070", "Orpheline"),
            )
        fun blocked(waitingOn: String? = null) =
            TaskOutcome("blocked", "La CI revient au parent", waitingOn = waitingOn)
        val activity =
            listOf(
                Run(
                    id = "proof-1",
                    taskId = "2db836ee-ce2e-552d-b9fa-2fef2030546c",
                    status = "succeeded",
                    outcome = blocked("user"),
                ),
                Run(
                    id = "ci-1",
                    taskId = "3c7c4abf-37d8-5123-814f-356a0797b145",
                    status = "succeeded",
                    finishedAt = 3,
                    outcome = blocked("requester"),
                    requestedBy =
                        RunRequester(
                            runId = "parent-run",
                            chatId = "parent",
                        ),
                ),
                Run(
                    id = "deploy-1",
                    taskId = "888f7003-e15b-5b1f-a1b3-a221bc92ec3a",
                    status = "succeeded",
                    outcome = blocked("external"),
                ),
                // Its requester is gone: the user is the one left to act.
                Run(
                    id = "orphan-1",
                    taskId = "0a0e9fbd-72fd-586d-b159-15ebbd5e1070",
                    status = "succeeded",
                    outcome = blocked("requester"),
                    requestedBy =
                        RunRequester(
                            runId = "gone",
                            chatId = "deleted",
                        ),
                ),
            )
        val feed =
            buildFeed(
                listOf(Chat("parent", title = "Orchestrer F8", updatedAt = 1)),
                activity,
                tasks,
            )
        assertEquals(listOf("run:proof-1", "run:orphan-1"), feed.forYou.map { it.key })
        assertTrue(feed.forYou.first().dismissible)
        val ci = feed.recent.first { it.key == "run:ci-1" }
        assertEquals(FeedKind.SET_ASIDE_TASK, ci.kind)
        assertEquals(SetAside.REQUESTER, ci.setAside)
        assertEquals("Tâche bloquée · En attente de Orchestrer F8", ci.subtitle)
        assertEquals(FeedRequester("Orchestrer F8", "parent", null), ci.requester)
        assertEquals(
            SetAside.EXTERNAL,
            feed.recent.first { it.key == "run:deploy-1" }.setAside,
        )
    }

    @Test
    fun `a dismissed mission leaves For you and a snoozed one waits for its time`() {
        val tasks =
            listOf(
                Task("8a025d72-4937-53c0-a41d-820414dc655d", "Preuve"),
                Task("00eada4d-84f3-57cb-837e-3c023a643647", "Notes"),
            )
        val activity =
            listOf(
                Run(
                    id = "r1",
                    taskId = "8a025d72-4937-53c0-a41d-820414dc655d",
                    status = "succeeded",
                    outcome = TaskOutcome("blocked", "CI"),
                    dismissal = OutcomeDismissal(),
                ),
                Run(
                    id = "r2",
                    taskId = "00eada4d-84f3-57cb-837e-3c023a643647",
                    status = "succeeded",
                    outcome = TaskOutcome("needs_input", "Choisir"),
                    dismissal = OutcomeDismissal(until = 100),
                ),
            )
        val before = buildFeed(emptyList(), activity, tasks, now = 50)
        assertTrue(before.forYou.isEmpty())
        assertEquals(SetAside.DISMISSED, before.recent.single().setAside)
        val snoozed = before.waiting.single()
        assertEquals(FeedKind.SNOOZED_TASK, snoozed.kind)
        assertEquals(100L, snoozed.wakeAt)
        val after = buildFeed(emptyList(), activity, tasks, now = 100)
        assertEquals(listOf("run:r2"), after.forYou.map { it.key })
        // Input the agent asked for can only be snoozed.
        assertTrue(!after.forYou.single().dismissible)
        // A local change shows before the next poll confirms it.
        val restored = withLocalDismissals(activity, mapOf("r1" to null))
        assertEquals(
            listOf("run:r1", "run:r2"),
            buildFeed(emptyList(), restored, tasks, now = 100).forYou.map { it.key },
        )
    }

    @Test
    fun `a snoozed mission asks again tomorrow at nine, local time`() {
        val zone = ZoneId.of("Europe/Paris")
        val evening = LocalDateTime.of(2026, 10, 20, 22, 30).atZone(zone).toInstant().toEpochMilli()
        assertEquals(
            LocalDateTime.of(2026, 10, 21, 9, 0).atZone(zone).toInstant().toEpochMilli(),
            tomorrowMorning(evening, zone),
        )
    }

    @Test
    fun `finished work not read yet waits in For you below what needs an answer`() {
        val chats =
            listOf(
                Chat(
                    "done",
                    "Notes de version",
                    agentName = "Rédacteur",
                    status = "succeeded",
                    finishedAt = 40,
                    updatedAt = 30,
                    unread = true,
                    preview = "J'ai publié les notes.",
                ),
                Chat("read", "Relecture", status = "succeeded", finishedAt = 50, updatedAt = 50),
                Chat("question", "Choisir", pendingQuestions = 1, updatedAt = 1),
            )
        val activity =
            listOf(
                Run(
                    id = "digest-1",
                    taskId = "22db892f-1069-55df-8435-9b003528bdcf",
                    status = "succeeded",
                    finishedAt = 60,
                    unread = true,
                    preview = "Trois sujets aujourd'hui.",
                )
            )
        val feed =
            buildFeed(
                chats,
                activity,
                listOf(Task("22db892f-1069-55df-8435-9b003528bdcf", "Veille")),
            )
        assertEquals(
            listOf(
                FeedKind.QUESTION to "Choisir",
                FeedKind.UNREAD_TASK to "Veille",
                FeedKind.UNREAD_CHAT to "Notes de version",
            ),
            feed.forYou.map { it.kind to it.title },
        )
        assertEquals(
            listOf("task:22db892f-1069-55df-8435-9b003528bdcf" to 60L, "chat:done" to 40L),
            feed.unread.map { it.read },
        )
        assertEquals("J'ai publié les notes.", feed.forYou.last().preview)
        assertEquals(listOf("read"), feed.recent.map { it.chatId })
        assertEquals(
            "1 élément pour vous · 2 résultats à lire",
            feedSummary(feed),
        )
        // Read on this device, before the server's lists catch up; a later turn is new again.
        val read = buildFeed(chats, activity, emptyList(), mapOf("chat:done" to 40L))
        assertEquals(setOf("read", "done"), read.recent.mapNotNull { it.chatId }.toSet())
        val later = buildFeed(chats, activity, emptyList(), mapOf("chat:done" to 39L))
        assertTrue(later.forYou.any { it.chatId == "done" && it.unread })
    }

    @Test
    fun `swipes move through conversations and missions in the order of the Fil`() {
        val chats =
            listOf(
                Chat("question", "Choisir", pendingQuestions = 1, updatedAt = 5),
                Chat("live", "Refonte", status = "running", updatedAt = 4),
                Chat("idle", "Relecture", updatedAt = 3),
            )
        val activity =
            listOf(
                Run(
                    id = "failed",
                    taskId = "8a025d72-4937-53c0-a41d-820414dc655d",
                    status = "failed",
                    finishedAt = 6,
                ),
                Run(
                    id = "running",
                    taskId = "00eada4d-84f3-57cb-837e-3c023a643647",
                    status = "running",
                    startedAt = 1,
                ),
            )
        val fil =
            buildFeed(
                    chats,
                    activity,
                    listOf(
                        Task("8a025d72-4937-53c0-a41d-820414dc655d", "Audit"),
                        Task("00eada4d-84f3-57cb-837e-3c023a643647", "Veille"),
                    ),
                )
                .keys()
        assertEquals(
            listOf("chat:question", "run:failed", "run:running", "chat:live", "chat:idle"),
            fil,
        )
        assertEquals(fil, pagerKeys(fil, listOf("run:failed"), 0))
        // A mission that finishes while read keeps its place among its neighbours.
        val finished = fil - "run:running"
        assertEquals(fil, pagerKeys(finished, fil, 2))
        // A page opened from outside the Fil does not swipe into it.
        assertEquals(listOf("run:elsewhere"), pagerKeys(fil, listOf("run:elsewhere"), 0))
    }

    @Test
    fun `read acknowledgement keeps the open conversation before the next unread page`() {
        val chats =
            listOf(
                Chat("a", "First result", unread = true, finishedAt = 30, updatedAt = 30),
                Chat("b", "Second result", unread = true, finishedAt = 20, updatedAt = 20),
                Chat("c", "Recent", updatedAt = 10),
            )
        val before = buildFeed(chats, emptyList(), emptyList()).keys()
        val after =
            buildFeed(
                    chats.map { if (it.id == "a") it.copy(unread = false) else it },
                    emptyList(),
                    emptyList(),
                )
                .keys()
        assertEquals(listOf("chat:b", "chat:a", "chat:c"), after)
        val held = pagerKeys(after, before, 0)
        assertEquals(listOf("chat:a", "chat:b", "chat:c"), held)
        assertEquals("chat:b", held[1])
    }

    @Test
    fun `navigation badge distinguishes action from reading and clears optimistic reads`() {
        val chats =
            listOf(
                Chat("a", "Result", unread = true, finishedAt = 30),
                Chat("b", "Question", pendingQuestions = 1),
            )
        assertEquals(FilAttention(1, 1), filAttention(buildFeed(chats, emptyList(), emptyList())))
        assertEquals(1, FilAttention(1, 4).count)
        assertEquals(4, FilAttention(0, 4).count)
        assertEquals(
            FilAttention(1, 0),
            filAttention(buildFeed(chats, emptyList(), emptyList(), mapOf("chat:a" to 30L))),
        )
    }

    @Test
    fun `summary and greeting describe the feed`() {
        assertEquals("Tout est calme.", feedSummary(Feed(emptyList(), emptyList(), emptyList())))
        val item = FeedItem("k", FeedKind.CHAT, "t", "s", "a", "a")
        assertEquals(
            "1 agent au travail · 2 éléments pour vous",
            feedSummary(Feed(listOf(item, item), listOf(item), emptyList())),
        )
        assertEquals("Bonjour.", greeting(9))
        assertEquals("Bonsoir.", greeting(21))
        assertEquals("Bonsoir.", greeting(3))
    }

    @Test
    fun `cron expressions are described in words or kept verbatim`() {
        assertEquals("Tous les jours · 09:00", describeCron("0 9 * * *"))
        assertEquals("Chaque lundi · 09:00", describeCron("0 9 * * 1"))
        assertEquals("Chaque dimanche · 18:30", describeCron("30 18 * * 0"))
        assertEquals("Chaque vendredi · 07:05", describeCron("5 7 * * FRI"))
        assertEquals("En semaine · 08:00", describeCron("0 8 * * 1-5"))
        assertEquals("Le week-end · 10:00", describeCron("0 10 * * 6,0"))
        assertEquals("Les lundi, jeudi · 09:00", describeCron("0 9 * * 1,4"))
        assertEquals("Le 1er du mois · 10:00", describeCron("0 10 1 * *"))
        assertEquals("Le 15 du mois · 06:00", describeCron("0 6 15 * *"))
        assertEquals("Toutes les heures", describeCron("0 * * * *"))
        assertEquals("Toutes les heures à :15", describeCron("15 * * * *"))
        assertEquals("Toutes les 15 minutes", describeCron("*/15 * * * *"))
        assertEquals("Chaque minute", describeCron("* * * * *"))
        assertEquals("Cron · 0 9 * 1 *", describeCron("0 9 * 1 *"))
        assertEquals("Cron · 0 9-17 * * *", describeCron("0 9-17 * * *"))
        assertEquals("Cron · @daily", describeCron("@daily"))
        assertEquals("Ponctuelle", describeSchedule(Task(cron = null)))
        assertEquals(
            "En pause · Tous les jours · 09:00",
            describeSchedule(Task(cron = "0 9 * * *", enabled = false)),
        )
        assertEquals("Archivée", describeSchedule(Task(cron = "0 9 * * *", archived = true)))
    }

    @Test
    fun `mission filters and ordering keep running and failing missions on top`() {
        val scheduled =
            Task(
                "e186f830-3e88-53ba-943f-b926bb27f37f",
                "B planifiée",
                cron = "0 9 * * *",
                nextRun = 200,
            )
        val soon =
            Task(
                "fd0107f8-a9c6-5b5e-ba66-5d3baf41211f",
                "C planifiée",
                cron = "0 8 * * *",
                nextRun = 100,
            )
        val once = Task("87d969a4-be62-5c7e-bacd-4917b8604bed", "A ponctuelle")
        val paused =
            Task(
                "5bfed223-205d-577b-a9eb-2561157d92ff",
                "En pause",
                cron = "0 9 * * *",
                enabled = false,
            )
        val archived =
            Task(
                "a8498a70-9bbc-557a-aa09-574c5d35021a",
                "Archivée",
                archived = true,
                enabled = false,
            )
        val all = listOf(scheduled, soon, once, paused, archived)
        assertEquals(
            listOf(
                "e186f830-3e88-53ba-943f-b926bb27f37f",
                "fd0107f8-a9c6-5b5e-ba66-5d3baf41211f",
                "87d969a4-be62-5c7e-bacd-4917b8604bed",
                "5bfed223-205d-577b-a9eb-2561157d92ff",
            ),
            all.filter { missionFilter(it, "all") }.map { it.id },
        )
        assertEquals(
            listOf("e186f830-3e88-53ba-943f-b926bb27f37f", "fd0107f8-a9c6-5b5e-ba66-5d3baf41211f"),
            all.filter { missionFilter(it, "scheduled") }.map { it.id },
        )
        assertEquals(
            listOf("87d969a4-be62-5c7e-bacd-4917b8604bed"),
            all.filter { missionFilter(it, "once") }.map { it.id },
        )
        assertEquals(
            listOf("5bfed223-205d-577b-a9eb-2561157d92ff"),
            all.filter { missionFilter(it, "paused") }.map { it.id },
        )
        assertEquals(
            listOf("a8498a70-9bbc-557a-aa09-574c5d35021a"),
            all.filter { missionFilter(it, "archived") }.map { it.id },
        )
        val latest =
            mapOf(
                "87d969a4-be62-5c7e-bacd-4917b8604bed" to
                    Run(
                        id = "r1",
                        taskId = "87d969a4-be62-5c7e-bacd-4917b8604bed",
                        status = "failed",
                    ),
                "5bfed223-205d-577b-a9eb-2561157d92ff" to
                    Run(
                        id = "r2",
                        taskId = "5bfed223-205d-577b-a9eb-2561157d92ff",
                        status = "running",
                    ),
            )
        assertEquals(
            listOf(
                "5bfed223-205d-577b-a9eb-2561157d92ff",
                "87d969a4-be62-5c7e-bacd-4917b8604bed",
                "fd0107f8-a9c6-5b5e-ba66-5d3baf41211f",
                "e186f830-3e88-53ba-943f-b926bb27f37f",
            ),
            missionOrder(listOf(scheduled, soon, once, paused), latest).map { it.id },
        )
    }

    @Test
    fun `search finds every kind of item with title matches first`() {
        val state =
            Workspace(
                agents =
                    listOf(
                        Agent(
                            exclusiveNodes = emptyList(),
                            plugins = emptyList(),
                            revision = "",
                            createdAt = 0,
                            access =
                                AccessPolicy(
                                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                    projects = null,
                                    skills = null,
                                    mcps = null,
                                ),
                            id = "5ca9d6c5-caa1-5234-8069-c22f7aa2da85",
                            name = "Revue de code",
                            description = "Relit les PR",
                        ),
                        Agent(
                            exclusiveNodes = emptyList(),
                            plugins = emptyList(),
                            revision = "",
                            createdAt = 0,
                            access =
                                AccessPolicy(
                                    mcpTools = build.cairn.contracts.McpTools(emptyMap()),
                                    projects = null,
                                    skills = null,
                                    mcps = null,
                                ),
                            id = "ae334e0d-2067-57a0-8791-222ba70e89e8",
                            name = "Ops",
                            description = "Revue des déploiements",
                        ),
                    ),
                projects =
                    listOf(
                        Project(
                            "5bfed223-205d-577b-a9eb-2561157d92ff",
                            "Cairn",
                            description = "Revue continue",
                        )
                    ),
                tasks =
                    listOf(
                        Task(
                            "7525d1be-5613-5715-96b1-145b0f8b9c78",
                            "Revue hebdomadaire",
                            cron = "0 9 * * 1",
                        ),
                        Task(
                            "d2de3a5c-f8c6-5276-b029-b4e459546f28",
                            "Revue archivée",
                            archived = true,
                        ),
                    ),
                skills = listOf(Skill("revue", "Guide de revue")),
            )
        val chats =
            listOf(
                Chat(
                    "c",
                    "Relecture PR",
                    agentName = "Revue de code",
                    agentId = "5ca9d6c5-caa1-5234-8069-c22f7aa2da85",
                ),
                Chat("22db892f-1069-55df-8435-9b003528bdcf", "Autre", agentName = "Cairn"),
            )
        val hits = searchWorkspace("revue", chats, state)
        assertEquals(
            setOf(
                SearchKind.CHAT,
                SearchKind.MISSION,
                SearchKind.AGENT,
                SearchKind.PROJECT,
                SearchKind.SKILL,
            ),
            hits.map { it.kind }.toSet(),
        )
        assertTrue(
            hits.none {
                it.id == "d2de3a5c-f8c6-5276-b029-b4e459546f28" ||
                    it.id == "22db892f-1069-55df-8435-9b003528bdcf"
            }
        )
        // Title matches first: the agent named "Revue de code" before "Ops" (description only).
        assertTrue(
            hits.indexOfFirst { it.id == "5ca9d6c5-caa1-5234-8069-c22f7aa2da85" } <
                hits.indexOfFirst { it.id == "ae334e0d-2067-57a0-8791-222ba70e89e8" }
        )
        assertEquals("Chaque lundi · 09:00", hits.first { it.kind == SearchKind.MISSION }.detail)
        assertEquals("Revue de code", hits.first { it.kind == SearchKind.CHAT }.avatar)
        assertTrue(searchWorkspace("   ", chats, state).isEmpty())
    }

    @Test
    fun `highlight marks the first case-insensitive match`() {
        val style = androidx.compose.ui.text.SpanStyle()
        val marked = highlight("Revue hebdomadaire", "HEBDO", style)
        assertEquals("Revue hebdomadaire", marked.text)
        assertEquals(6, marked.spanStyles.single().start)
        assertEquals(11, marked.spanStyles.single().end)
        assertTrue(highlight("Autre", "revue", style).spanStyles.isEmpty())
    }

    @Test
    fun `stamps and elapsed times are short and relative`() {
        val now = at(2026, 9, 25, 14, 0)
        assertEquals("09:30", shortStamp(at(2026, 9, 25, 9, 30), now, zone))
        assertEquals("Hier", shortStamp(at(2026, 9, 24, 22, 0), now, zone))
        assertEquals("Lun.", shortStamp(at(2026, 9, 21, 9, 0), now, zone))
        assertEquals("3 août", shortStamp(at(2026, 8, 3, 9, 0), now, zone))
        assertEquals("3 août 2025", shortStamp(at(2025, 8, 3, 9, 0), now, zone))
        assertEquals("", shortStamp(0, now, zone))
        assertEquals("Aujourd’hui · 18:00", upcomingStamp(at(2026, 9, 25, 18, 0), now, zone))
        assertEquals("Demain · 09:00", upcomingStamp(at(2026, 9, 26, 9, 0), now, zone))
        assertEquals("lun. 28 sept. · 09:00", upcomingStamp(at(2026, 9, 28, 9, 0), now, zone))
        assertEquals("ven. 1 janv. 2027 · 09:00", upcomingStamp(at(2027, 1, 1, 9, 0), now, zone))
        assertEquals("< 1 min", elapsed(now - 20_000, now))
        assertEquals("14 min", elapsed(now - 14 * 60_000, now))
        assertEquals("2 h 05", elapsed(now - 125 * 60_000, now))
        assertEquals("", elapsed(null, now))
    }

    @Test
    fun `dock selection follows the section a screen belongs to`() {
        assertEquals("fil", dockSelection("fil"))
        assertEquals("fil", dockSelection("chat/{id}"))
        // A mission opened from the Fil reads beside its conversations.
        assertEquals("fil", dockSelection("fil/run/{id}"))
        assertEquals("fil", dockSelection("new-chat?agent={agent}&project={project}"))
        assertEquals("fil", dockSelection("search"))
        assertEquals("missions", dockSelection("missions"))
        assertEquals("missions", dockSelection("run/{id}"))
        assertEquals("atelier", dockSelection("atelier"))
        assertEquals("atelier", dockSelection("connections"))
        assertEquals("atelier", dockSelection("runs"))
    }

    @Test
    fun `connection summary reports coding agents without a usable account and the best capacity`() {
        val codex =
            listOf(
                Account(id = "a", provider = "codex", name = "Perso", state = "ready", remainingPercent = 40.0),
                Account(id = "b", provider = "codex", name = "Pro", state = "ready", remainingPercent = 70.0),
                Account(id = "c", provider = "codex", name = "Off",
                    enabled = false,
                    state = "ready",
                    remainingPercent = 99.0,
                ),
            )
        val summary =
            ConnectionSummary(
                setOf("codex", "claude"),
                AccountsView(codex, required = listOf("claude")),
            )
        assertEquals(listOf("claude"), summary.missing)
        assertEquals(2, summary.ready("codex").size)
        assertEquals(3, summary.total("codex"))
        assertEquals(70.0, summary.remaining("codex")!!, 0.0)
        // The server says which coding agents wait for the user; only those in use matter.
        assertEquals(
            emptyList<String>(),
            ConnectionSummary(setOf("codex"), AccountsView(required = listOf("claude"))).missing,
        )
        // Unknown state is not reported as missing.
        assertEquals(emptyList<String>(), ConnectionSummary(setOf("claude"), null).missing)
        assertNull(ConnectionSummary(setOf("codex"), null).remaining("codex"))
    }

    @Test
    fun `account statuses, windows and resets read naturally in French`() {
        val now = 1_790_000_000_000
        assertEquals("Prochain", accountStatusLabel(Account(id = "a", name = "A", status = "next")))
        assertNull(accountStatusLabel(Account(id = "a", name = "A", status = "ready")))
        assertEquals(
            "Reprise dans 48 min",
            accountStatusLabel(
                Account(id = "a", name = "A", status = "waiting", resetsAt = now / 1000 + 48 * 60),
                now,
            ),
        )
        assertEquals("dans 2 h 05", resetsIn(now / 1000 + 125 * 60, now))
        assertEquals("maintenant", resetsIn(now / 1000 - 5, now))
        assertEquals(
            "5 heures",
            windowLabel(AccountWindow(id = "w", label = "5-hour window", durationMins = 300)),
        )
        assertEquals("Semaine", windowLabel(AccountWindow(id = "w", label = "Weekly", durationMins = 10080)))
        assertEquals(
            "Semaine · Opus",
            windowLabel(
                AccountWindow(id = "w", label = "Weekly · Opus", durationMins = 10080, models = listOf("opus"))
            ),
        )
        assertEquals(40.0, AccountWindow(id = "w", usedPercent = 60.0).remaining, 0.0)
        assertEquals(0.0, AccountWindow(id = "w", usedPercent = 105.0).remaining, 0.0)
        val usage =
            AccountUsage(
                windows = listOf(
                    AccountWindow(id = "week", durationMins = 10080),
                    AccountWindow(id = "opus", models = listOf("opus")),
                    AccountWindow(id = "five", durationMins = 300),
                )
            )
        assertEquals(listOf("five", "week"), usage.general.map { it.id })
    }

    @Test
    fun `identity colours and initials are stable`() {
        assertEquals(identityColor("agent-1"), identityColor("agent-1"))
        assertEquals("R", initial("revue de code"))
        assertEquals("C", initial("  « Cairn »"))
        assertEquals("?", initial("   "))
    }
}
