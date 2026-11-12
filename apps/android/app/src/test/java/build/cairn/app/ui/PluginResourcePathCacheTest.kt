package build.cairn.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PluginResourcePathCacheTest {
    @Test
    fun cacheRemovesExpiredPathsAndKeepsItsSizeBounded() {
        var time = 0L
        val cache = PluginResourcePathCache(maxEntries = 2, expirationMs = 10) { time }

        cache.put("expired", "Expired path")
        time = 10
        cache.put("first", "First path")

        assertNull(cache.get("expired"))
        assertEquals(1, cache.size)

        cache.put("second", "Second path")
        cache.put("third", "Third path")

        assertEquals(2, cache.size)
        assertNull(cache.get("first"))
        assertEquals("Second path", cache.get("second"))
        assertEquals("Third path", cache.get("third"))
    }
}
