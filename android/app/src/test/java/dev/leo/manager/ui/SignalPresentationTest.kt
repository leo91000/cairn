package dev.leo.manager.ui

import dev.leo.manager.data.*
import java.time.LocalDateTime
import java.time.ZoneId
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SignalPresentationTest {
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
                    agentName = "Leo",
                    projectName = "Manager",
                    status = "running",
                    run = Run("r1", status = "running", startedAt = 100),
                    updatedAt = 4,
                ),
                Chat("queued", "En file", agentName = "Leo", status = "queued", updatedAt = 3),
                Chat(
                    "paused",
                    "Mise en pause",
                    agentName = "Leo",
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
                Chat("idle", "Ancienne conversation", agentName = "Leo", updatedAt = 0),
            )
        val tasks =
            listOf(
                Task("daily", "Revue quotidienne", agentId = "agent"),
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
                    snapshot = Snapshot(agent = Agent("agent", "Reviewer")),
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
        assertEquals("Leo · Manager", feed.running.first().subtitle)
        assertEquals("En attente · Leo", feed.running.last().subtitle)
        // Only work that is running animates; queued work keeps its static badge.
        assertEquals(listOf(true, false), feed.running.map { it.working })
        assertEquals(listOf("chat:paused", "chat:idle"), feed.recent.map { it.key })
        assertTrue(feed.recent.first().subtitle.startsWith("En pause"))
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
        val paused = Task("p", "En pause", cron = "0 9 * * *", enabled = false)
        val archived = Task("x", "Archivée", archived = true, enabled = false)
        val all = listOf(scheduled, soon, once, paused, archived)
        assertEquals(
            listOf("s", "n", "o", "p"),
            all.filter { missionFilter(it, "all") }.map { it.id },
        )
        assertEquals(listOf("s", "n"), all.filter { missionFilter(it, "scheduled") }.map { it.id })
        assertEquals(listOf("o"), all.filter { missionFilter(it, "once") }.map { it.id })
        assertEquals(listOf("p"), all.filter { missionFilter(it, "paused") }.map { it.id })
        assertEquals(listOf("x"), all.filter { missionFilter(it, "archived") }.map { it.id })
        val latest =
            mapOf(
                "o" to Run("r1", taskId = "o", status = "failed"),
                "p" to Run("r2", taskId = "p", status = "running"),
            )
        assertEquals(
            listOf("p", "o", "n", "s"),
            missionOrder(listOf(scheduled, soon, once, paused), latest).map { it.id },
        )
    }

    @Test
    fun `search finds every kind of item with title matches first`() {
        val state =
            Workspace(
                agents =
                    listOf(
                        Agent("rev", "Revue de code", description = "Relit les PR"),
                        Agent("ops", "Ops", description = "Revue des déploiements"),
                    ),
                projects =
                    listOf(Project("p", "Leo Agent Manager", description = "Revue continue")),
                tasks =
                    listOf(
                        Task("t", "Revue hebdomadaire", cron = "0 9 * * 1"),
                        Task("z", "Revue archivée", archived = true),
                    ),
                skills = listOf(Skill("revue", "Guide de revue")),
            )
        val chats =
            listOf(
                Chat("c", "Relecture PR", agentName = "Revue de code", agentId = "rev"),
                Chat("d", "Autre", agentName = "Leo"),
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
        assertTrue(hits.indexOfFirst { it.id == "rev" } < hits.indexOfFirst { it.id == "ops" })
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
        assertEquals("L", initial("  « Leo »"))
        assertEquals("?", initial("   "))
    }
}
