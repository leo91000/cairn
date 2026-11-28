package build.cairn.app.ui

import build.cairn.app.data.*
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
        val before = buildFeed(chats)
        assertEquals(setOf("question", "error"), before.unread.map { it.chatId }.toSet())
        val after =
            buildFeed(
                chats,
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
        val feed = buildFeed(listOf(delegated))
        assertTrue(feed.unread.isEmpty())
        assertEquals("child", feed.all.single().chatId)
    }
}
