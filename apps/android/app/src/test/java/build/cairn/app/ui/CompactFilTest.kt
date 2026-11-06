package build.cairn.app.ui

import build.cairn.app.data.*
import build.cairn.contracts.ChatRequester
import build.cairn.contracts.ConversationRequester
import org.junit.Assert.*
import org.junit.Test

class CompactFilTest {
    @Test
    fun `reading acknowledges errors and questions independently of their status`() {
        val chats =
            listOf(
                Chat(
                    id = "question",
                    pendingQuestions = 1,
                    status = "running",
                    unread = true,
                    readAt = 10,
                ),
                Chat(id = "error", status = "failed", unread = true, readAt = 20, finishedAt = 20),
                Chat(id = "working", status = "running"),
            )
        val before = buildFeed(chats, emptyList(), emptyList())
        assertEquals(setOf("question", "error"), before.unread.map { it.chatId }.toSet())
        val after =
            buildFeed(
                chats,
                emptyList(),
                emptyList(),
                mapOf("chat:question" to 10L, "chat:error" to 20L),
            )
        assertTrue(after.unread.isEmpty())
        assertEquals(setOf("question", "working"), after.ongoing.map { it.chatId }.toSet())
        assertEquals(FeedKind.FAILED_CHAT, after.all.first { it.chatId == "error" }.kind)
        assertEquals(0, filAttention(after).count)
        assertEquals("En cours", after.ongoing.first { it.chatId == "working" }.statusLabel)
        assertEquals("Répondre", after.ongoing.first { it.chatId == "question" }.statusLabel)
        val newer =
            buildFeed(
                listOf(chats.first().copy(readAt = 30)),
                emptyList(),
                emptyList(),
                mapOf("chat:question" to 10L),
            )
        assertEquals(1, newer.unread.size)
        assertEquals("chat:question" to 30L, newer.unread.single().read)
    }

    @Test
    fun `completed delegated work stays available without a notification`() {
        val delegated =
            Chat(
                id = "child",
                status = "succeeded",
                unread = true,
                readAt = 20,
                finishedAt = 20,
                requestedBy = ConversationRequester(chatId = "parent", runId = "run"),
            )
        val feed = buildFeed(listOf(delegated), emptyList(), emptyList())
        assertTrue(feed.unread.isEmpty())
        assertEquals("child", feed.all.single().chatId)
    }

    @Test
    fun `read mission results stay in all and an input request stays ongoing`() {
        val runs =
            listOf(
                Run(
                    id = "done",
                    taskId = "mission",
                    trigger = "manual",
                    status = "succeeded",
                    unread = true,
                    finishedAt = 20,
                ),
                Run(
                    id = "input",
                    taskId = "input",
                    trigger = "manual",
                    status = "succeeded",
                    unread = true,
                    finishedAt = 30,
                    outcome = TaskOutcome("needs_input", "Pick one"),
                ),
            )
        val feed =
            buildFeed(
                emptyList(),
                runs,
                emptyList(),
                mapOf("task:mission" to 20L, "task:input" to 30L),
            )
        assertTrue(feed.unread.isEmpty())
        assertEquals(listOf("input"), feed.ongoing.map { it.taskId })
        assertEquals(setOf("mission", "input"), feed.all.map { it.taskId }.toSet())
    }

    @Test
    fun `delegated work sits below the work that started it when both are listed`() {
        val requester = ChatRequester(chatId = "ship", title = "Ship PR 382")
        val chats =
            listOf(
                Chat(id = "ship", title = "Ship PR 382", status = "succeeded", updatedAt = 10),
                Chat(
                    id = "review",
                    title = "Review R51",
                    status = "succeeded",
                    updatedAt = 30,
                    requester = requester,
                ),
                Chat(id = "other", title = "Other work", status = "succeeded", updatedAt = 20),
                Chat(
                    id = "orphan",
                    title = "Orphan review",
                    status = "succeeded",
                    updatedAt = 5,
                    requester = ChatRequester(chatId = "gone", title = "Deleted"),
                ),
            )
        val feed = buildFeed(chats, emptyList(), emptyList())
        assertEquals(
            listOf(
                "Other work" to false,
                "Ship PR 382" to false,
                "Review R51" to true,
                "Orphan review" to false,
            ),
            threadFeed(feed.all).map { (item, nested) -> item.title to nested },
        )
        // Without its requester in the view, the row stays in place and names it.
        assertEquals(
            listOf("Review R51" to false),
            threadFeed(feed.all.filter { it.chatId == "review" }).map { (item, nested) ->
                item.title to nested
            },
        )
    }

    @Test
    fun `finished work is marked quiet unlike work that needs attention`() {
        val feed =
            buildFeed(
                listOf(
                    Chat(id = "done", status = "succeeded"),
                    Chat(id = "failed", status = "failed"),
                    Chat(id = "paused", status = "succeeded", paused = true),
                    Chat(id = "unread", status = "succeeded", unread = true, finishedAt = 20),
                ),
                listOf(
                    Run(id = "run", taskId = "mission", trigger = "manual", status = "succeeded")
                ),
                emptyList(),
            )
        assertEquals(
            mapOf(
                "chat:done" to true,
                "chat:failed" to false,
                "chat:paused" to false,
                "chat:unread" to true,
                "run:run" to true,
            ),
            feed.all.associate { it.key to it.finished },
        )
    }

    @Test
    fun `quiet missions fold together when two or more finished and were read in a row`() {
        fun run(
            task: String,
            finishedAt: Long,
            status: String = "succeeded",
            unread: Boolean = false,
        ) =
            Run(
                id = "$task-run",
                taskId = task,
                trigger = "schedule",
                status = status,
                finishedAt = finishedAt,
                unread = unread,
            )
        val feed =
            buildFeed(
                listOf(Chat(id = "talk", title = "talk", status = "succeeded", updatedAt = 25)),
                listOf(
                    run("a", 50),
                    run("b", 40),
                    run("c", 30),
                    run("d", 20),
                    run("e", 10, status = "failed"),
                    run("f", 5, unread = true),
                    run("g", 4),
                ),
                emptyList(),
            )
        val entries =
            foldFeed(threadFeed(feed.all)).map {
                when (it) {
                    is FeedEntry.Fold -> it.items.map { item -> item.taskId }
                    is FeedEntry.Line -> it.item.taskId ?: it.item.chatId
                }
            }
        // A lone quiet mission, a failure and unread results stay rows.
        assertEquals(listOf(listOf("a", "b", "c"), "talk", "d", "e", "f", "g"), entries)
    }
}
