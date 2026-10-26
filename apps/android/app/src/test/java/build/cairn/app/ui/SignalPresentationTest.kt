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
            Run("mission", taskId = "task", status = "running", suspendedByExclusive = reservation)
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
                    run = Run("r1", status = "running", startedAt = 100),
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
                            "r2",
                            status = "queued",
                            accountWaitReason =
                                "Connect a Claude Code account in Connections before running this agent.",
                            accountRequired = "claude",
                        ),
                ),
                Chat("idle", "Ancienne conversation", agentName = "Cairn", updatedAt = 0),
            )
        val tasks =
            listOf(
                Task(
                    "daily",
                    "Revue quotidienne",
                    agentId = "d3153f9f-724b-59c3-9614-ab78f231c4d3",
                ),
                Task("old", "Archivée", archived = true),
            )
        val activity =
            listOf(
                Run(
                    "t1",
                    taskId = "daily",
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
                                )
                        ),
                ),
                Run("t2", taskId = "old", status = "failed", trigger = "manual"),
                Run("t3", taskId = "chat-task", status = "failed", trigger = "chat"),
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
        assertEquals("daily", failedTask.taskId)
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
                    Run("audit-run", taskId = "audit", status = "running", provider = "opencode")
                ),
                listOf(Task("audit", "Audit")),
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
        val tasks = listOf(Task("a", "Audit"), Task("b", "Veille"))
        val activity =
            listOf(
                Run(
                    "r1",
                    taskId = "a",
                    status = "succeeded",
                    outcome = TaskOutcome("blocked", "Accès manquant"),
                ),
                Run("r2", taskId = "b", status = "running", startedAt = 10),
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
                Task("proof", "Preuve appareil"),
                Task("ci", "Checks PR315"),
                Task("deploy", "Déploiement staging"),
                Task("orphan", "Orpheline"),
            )
        fun blocked(waitingOn: String? = null) =
            TaskOutcome("blocked", "La CI revient au parent", waitingOn = waitingOn)
        val activity =
            listOf(
                Run("proof-1", taskId = "proof", status = "succeeded", outcome = blocked("user")),
                Run(
                    "ci-1",
                    taskId = "ci",
                    status = "succeeded",
                    finishedAt = 3,
                    outcome = blocked("requester"),
                    requestedBy = RunRequester("parent-run", chatId = "parent"),
                ),
                Run(
                    "deploy-1",
                    taskId = "deploy",
                    status = "succeeded",
                    outcome = blocked("external"),
                ),
                // Its requester is gone: the user is the one left to act.
                Run(
                    "orphan-1",
                    taskId = "orphan",
                    status = "succeeded",
                    outcome = blocked("requester"),
                    requestedBy = RunRequester("gone", chatId = "deleted"),
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
        val tasks = listOf(Task("a", "Preuve"), Task("b", "Notes"))
        val activity =
            listOf(
                Run(
                    "r1",
                    taskId = "a",
                    status = "succeeded",
                    outcome = TaskOutcome("blocked", "CI"),
                    dismissal = OutcomeDismissal(),
                ),
                Run(
                    "r2",
                    taskId = "b",
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
                    "digest-1",
                    taskId = "d",
                    status = "succeeded",
                    finishedAt = 60,
                    unread = true,
                    preview = "Trois sujets aujourd'hui.",
                )
            )
        val feed = buildFeed(chats, activity, listOf(Task("d", "Veille")))
        assertEquals(
            listOf(
                FeedKind.QUESTION to "Choisir",
                FeedKind.UNREAD_TASK to "Veille",
                FeedKind.UNREAD_CHAT to "Notes de version",
            ),
            feed.forYou.map { it.kind to it.title },
        )
        assertEquals(listOf("task:d" to 60L, "chat:done" to 40L), feed.unread.map { it.read })
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
                Run("failed", taskId = "a", status = "failed", finishedAt = 6),
                Run("running", taskId = "b", status = "running", startedAt = 1),
            )
        val fil = buildFeed(chats, activity, listOf(Task("a", "Audit"), Task("b", "Veille"))).keys()
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
        val scheduled = Task("s", "B planifiée", cron = "0 9 * * *", nextRun = 200)
        val soon = Task("n", "C planifiée", cron = "0 8 * * *", nextRun = 100)
        val once = Task("o", "A ponctuelle")
        val paused =
            Task(
                "5bfed223-205d-577b-a9eb-2561157d92ff",
                "En pause",
                cron = "0 9 * * *",
                enabled = false,
            )
        val archived = Task("x", "Archivée", archived = true, enabled = false)
        val all = listOf(scheduled, soon, once, paused, archived)
        assertEquals(
            listOf("s", "n", "o", "5bfed223-205d-577b-a9eb-2561157d92ff"),
            all.filter { missionFilter(it, "all") }.map { it.id },
        )
        assertEquals(listOf("s", "n"), all.filter { missionFilter(it, "scheduled") }.map { it.id })
        assertEquals(listOf("o"), all.filter { missionFilter(it, "once") }.map { it.id })
        assertEquals(
            listOf("5bfed223-205d-577b-a9eb-2561157d92ff"),
            all.filter { missionFilter(it, "paused") }.map { it.id },
        )
        assertEquals(listOf("x"), all.filter { missionFilter(it, "archived") }.map { it.id })
        val latest =
            mapOf(
                "o" to Run("r1", taskId = "o", status = "failed"),
                "5bfed223-205d-577b-a9eb-2561157d92ff" to
                    Run("r2", taskId = "5bfed223-205d-577b-a9eb-2561157d92ff", status = "running"),
            )
        assertEquals(
            listOf("5bfed223-205d-577b-a9eb-2561157d92ff", "o", "n", "s"),
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
                        Task("t", "Revue hebdomadaire", cron = "0 9 * * 1"),
                        Task("z", "Revue archivée", archived = true),
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
                Chat("d", "Autre", agentName = "Cairn"),
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
        assertTrue(hits.none { it.id == "z" || it.id == "d" })
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
                Account("a", "codex", "Perso", state = "ready", remainingPercent = 40.0),
                Account("b", "codex", "Pro", state = "ready", remainingPercent = 70.0),
                Account(
                    "c",
                    "codex",
                    "Off",
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
        assertEquals("Prochain", accountStatusLabel(Account("a", name = "A", status = "next")))
        assertNull(accountStatusLabel(Account("a", name = "A", status = "ready")))
        assertEquals(
            "Reprise dans 48 min",
            accountStatusLabel(
                Account("a", name = "A", status = "waiting", resetsAt = now / 1000 + 48 * 60),
                now,
            ),
        )
        assertEquals("dans 2 h 05", resetsIn(now / 1000 + 125 * 60, now))
        assertEquals("maintenant", resetsIn(now / 1000 - 5, now))
        assertEquals(
            "5 heures",
            windowLabel(AccountWindow("w", "5-hour window", durationMins = 300)),
        )
        assertEquals("Semaine", windowLabel(AccountWindow("w", "Weekly", durationMins = 10080)))
        assertEquals(
            "Semaine · Opus",
            windowLabel(
                AccountWindow("w", "Weekly · Opus", durationMins = 10080, models = listOf("opus"))
            ),
        )
        assertEquals(40.0, AccountWindow("w", usedPercent = 60.0).remaining, 0.0)
        assertEquals(0.0, AccountWindow("w", usedPercent = 105.0).remaining, 0.0)
        val usage =
            AccountUsage(
                listOf(
                    AccountWindow("week", durationMins = 10080),
                    AccountWindow("opus", models = listOf("opus")),
                    AccountWindow("five", durationMins = 300),
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
