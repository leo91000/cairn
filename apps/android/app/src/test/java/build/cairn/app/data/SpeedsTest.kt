package build.cairn.app.data

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class SpeedsTest {
    private val fast = ServiceTier("priority", "Fast", "2x speed, increased usage")
    private val astra = CodingAgentModel("gpt-6-astra", serviceTiers = listOf(fast))

    @Test
    fun usesTheChosenSpeedThenTheInheritedOneThenStandard() {
        assertEquals(STANDARD_SPEED, effectiveSpeed("", ""))
        assertEquals("priority", effectiveSpeed("", "priority"))
        assertEquals(STANDARD_SPEED, effectiveSpeed(STANDARD_SPEED, "priority"))
    }

    @Test
    fun namesAQueuedSpeedAsItsModelRunsIt() {
        val fast = astra.copy(model = "fast")
        val deep = astra.copy(model = "deep", serviceTiers = emptyList())
        assertEquals("vitesse fast", speedNote("priority", "fast", listOf(fast, deep)))
        assertEquals(
            "vitesse standard (fast non proposée)",
            speedNote("priority", "deep", listOf(fast, deep)),
        )
        // A model the catalog does not list leaves the check to Codex.
        assertEquals("vitesse fast", speedNote("priority", "custom", listOf(fast, deep)))
        assertNull(speedNote("", "deep", listOf(fast, deep)))
    }

    @Test
    fun treatsCatalogsWithoutSpeedsAsUnknownRatherThanUnsupported() {
        assertTrue(offersSpeed(astra, "priority"))
        assertFalse(offersSpeed(astra, "ultrafast"))
        assertFalse(offersSpeed(astra.copy(serviceTiers = emptyList()), "priority"))
        assertTrue(offersSpeed(astra.copy(serviceTiers = null), "priority"))
        assertTrue(offersSpeed(null, "priority"))
        assertTrue(offersSpeed(astra.copy(serviceTiers = emptyList()), STANDARD_SPEED))
    }

    @Test
    fun offersSpeedsOnlyForCodexModelsListingThemOrToKeepASavedChoice() {
        assertTrue(showsSpeed("codex", astra, ""))
        assertFalse(showsSpeed("codex", astra.copy(serviceTiers = emptyList()), ""))
        assertTrue(showsSpeed("codex", astra.copy(serviceTiers = null), "priority"))
        assertFalse(showsSpeed("claude", astra, "priority"))
        assertFalse(showsSpeed("opencode", astra, ""))
    }

    @Test
    fun namesSpeedsWithCodexOwnWords() {
        assertEquals("Fast", speedLabel("priority", listOf(fast)))
        assertEquals("Standard", speedLabel(STANDARD_SPEED))
        assertEquals("Ultra fast", speedLabel("ultra_fast"))
        assertEquals("Par défaut", speedLabel(""))
    }

    @Test
    fun decodesCatalogsWithAndWithoutSpeedsAndOlderMessages() {
        val json = Json { ignoreUnknownKeys = true }
        val current =
            json.decodeFromString<CodingAgentModel>(
                """{"model":"gpt-6-astra","serviceTiers":[{"id":"priority","name":"Fast","description":"2x speed, increased usage","limited":true}],"defaultServiceTier":null}"""
            )
        assertEquals(listOf(fast.copy(limited = true)), current.serviceTiers)
        val older = json.decodeFromString<CodingAgentModel>("""{"model":"gpt-5.5"}""")
        assertNull(older.serviceTiers)
        assertNull(older.defaultServiceTier)
        val message = json.decodeFromString<ChatMessage>("""{"id":"m","text":"Hi"}""")
        assertEquals("", message.speed)
        assertEquals("", json.decodeFromString<Agent>("""{"id":"a"}""").speed)
    }
}
