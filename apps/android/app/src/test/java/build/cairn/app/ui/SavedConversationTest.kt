package build.cairn.app.ui

import build.cairn.app.data.*
import org.junit.Assert.*
import org.junit.Test

/** Mirrors the web's `savedIsCurrent`: which saved conversations open before their stream. */
class SavedConversationTest {
    private val run =
        Run(
            id = "run",
            status = "succeeded",
            finishedAt = 20,
            taskId = "",
        )
    private val chat = Chat(id = "chat", runId = "run", updatedAt = 10, status = "succeeded")

    private fun saved(run: Run? = this.run, chat: Chat? = this.chat) =
        CachedHistory(
            cursor = 1,
            history = "v2:run:1",
            state = LiveState(run = run, chat = chat),
            events = emptyList(),
        )

    private fun listed(chat: Chat = this.chat) = listOf(chat.copy(finishedAt = 20))

    @Test
    fun `a copy matching the Fil is current`() {
        assertTrue(savedIsCurrent(saved(), listed()))
    }

    @Test
    fun `without the Fil or the conversation in it, the copy shows`() {
        assertTrue(savedIsCurrent(saved(), null))
        assertTrue(savedIsCurrent(saved(), emptyList()))
        assertTrue(savedIsCurrent(saved(chat = null), listed()))
    }

    @Test
    fun `a later turn, a change or work in progress make it outdated`() {
        assertFalse(savedIsCurrent(saved(), listOf(chat.copy(finishedAt = 30))))
        assertFalse(savedIsCurrent(saved(), listed(chat.copy(updatedAt = 11))))
        assertFalse(savedIsCurrent(saved(), listed(chat.copy(runId = "other"))))
        val working = run.copy(status = "running", finishedAt = null)
        assertFalse(
            savedIsCurrent(
                saved(run = working),
                listOf(chat.copy(status = "running", finishedAt = null)),
            )
        )
    }
}
