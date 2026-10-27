package build.cairn.app.data

import org.junit.Assert.*
import org.junit.Test

class OlderPagesTest {
    private fun event(id: Long) = RunEvent(id = id,
createdAt = id,
type = "chat.user",
text = "Message $id",
runId = "r1")

    private fun snapshot() =
        LiveSnapshot(
            events = (10L..12L).map(::event),
            history = "h1",
            epoch = 3,
            oldest = 10,
            hasOlder = true,
            cursor = 12,
        )

    private fun page() = HistoryPage((5L..9L).map(::event), "h1", 5, true)

    @Test
    fun `a page from a replaced history or epoch is rejected without altering current pages`() {
        for (replacement in listOf(snapshot().copy(history = "h2"), snapshot().copy(epoch = 4))) {
            val pages = OlderPages()
            assertNotNull(pages.apply(page(), snapshot(), snapshot()))
            assertNull(pages.apply(page(), replacement, snapshot()))
            assertEquals(replacement, pages.display(replacement))
            assertNull(pages.apply(page().copy(history = "h2"), snapshot(), snapshot()))
        }
    }

    @Test
    fun `a page requested before a live emission preserves the latest stream cursor and rows`() {
        val pages = OlderPages()
        val requested = snapshot()
        val current = requested.copy(events = (10L..13L).map(::event), cursor = 13)
        val applied = checkNotNull(pages.apply(page(), current, requested))
        assertEquals((5L..13L).toList(), applied.events.map { it.id })
        assertEquals(13L, applied.cursor)
    }

    @Test
    fun `stream emissions preserve accepted pages and paging metadata until replacement`() {
        val pages = OlderPages()
        val initial = snapshot()
        val applied = checkNotNull(pages.apply(page(), initial, initial))
        assertEquals((5L..12L).toList(), applied.events.map { it.id })
        assertEquals(5L, applied.oldest)
        assertTrue(applied.hasOlder)
        assertEquals(12L, applied.cursor)

        val next = initial.copy(events = (10L..13L).map(::event), cursor = 13)
        val displayed = pages.display(next)
        assertEquals((5L..13L).toList(), displayed.events.map { it.id })
        assertEquals(5L, displayed.oldest)
        assertEquals(13L, displayed.cursor)

        val replaced = next.copy(epoch = 4)
        assertEquals(replaced, pages.display(replaced))
    }

    @Test
    fun `restored pages and a new page retain all events through later revalidation`() {
        val pages = OlderPages()
        val restored = snapshot().copy(events = (5L..12L).map(::event), oldest = 5)
        assertEquals(restored, pages.display(restored))
        val applied =
            checkNotNull(
                pages.apply(HistoryPage((1L..4L).map(::event), "h1", 1, false), restored, restored)
            )
        val revalidated = pages.display(restored.copy(cursor = 13))
        assertEquals((1L..12L).toList(), applied.events.map { it.id })
        assertEquals(applied.events, revalidated.events)
        assertEquals(1L, revalidated.oldest)
        assertFalse(revalidated.hasOlder)
    }

    @Test
    fun `a non progressing page fails atomically and the request can be retried`() {
        val pages = OlderPages()
        val initial = snapshot()
        assertThrows(IllegalArgumentException::class.java) {
            pages.apply(page().copy(oldest = 10), initial, initial)
        }
        assertEquals(initial, pages.display(initial))
        assertNotNull(pages.apply(page(), initial, initial))
    }
}
