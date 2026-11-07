package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ContextUsageTest {
    @Test
    fun `the latest request is shown against the reported window`() {
        val summary =
            contextSummary(
                ContextUsage(
                    contextTokens = 42_202,
                    contextWindow = 200_000,
                    cachedTokens = 40_002,
                    estimated = false,
                )
            )
        assertEquals(21, summary.percent)
        assertEquals("42 k / 200 k · 21 %", summary.short)
        assertEquals("42 202 sur 200 000 jetons (21 %)", summary.label)
        assertEquals(
            listOf(
                "Mesuré sur la dernière requête du modèle",
                "Fenêtre de 200 000 jetons",
                "40 002 jetons lus depuis le cache",
            ),
            summary.details,
        )
    }

    @Test
    fun `an unknown window and missing reports stay unavailable`() {
        val unknown =
            contextSummary(
                ContextUsage(
                    contextTokens = 12_000,
                    contextWindow = null,
                    cachedTokens = null,
                    estimated = false,
                )
            )
        assertNull(unknown.percent)
        assertEquals("12 k jetons", unknown.short)
        assertEquals("12 000 jetons · fenêtre inconnue", unknown.label)
        assertEquals("Taille de fenêtre non communiquée", unknown.details[1])
        for (usage in listOf(null, ContextUsage(contextTokens = 0, contextWindow = 200_000))) {
            val missing = contextSummary(usage)
            assertNull(missing.percent)
            assertEquals("Utilisation du contexte indisponible", missing.label)
        }
        val estimate =
            contextSummary(
                ContextUsage(contextTokens = 6_200, contextWindow = 1_000_000, estimated = true)
            )
        assertEquals("≈ 6 k / 1 M · 1 %", estimate.short)
        assertEquals("Estimation de l’agent", estimate.details.first())
    }

    @Test
    fun `older servers without context fields still decode`() {
        val chat = wireJson.decodeFromString<Chat>("{\"id\":\"chat\"}")
        assertNull(chat.contextUsage)
        assertNull(chat.compaction)
        val current =
            wireJson.decodeFromString<Chat>(
                "{\"id\":\"chat\",\"contextUsage\":{\"contextTokens\":5,\"contextWindow\":null,\"cachedTokens\":null,\"estimated\":true},\"compaction\":{\"supported\":true,\"running\":false,\"available\":false,\"reason\":\"Wait for the current turn to finish before compacting.\"}}"
            )
        assertEquals(
            ContextUsage(
                contextTokens = 5,
                contextWindow = null,
                cachedTokens = null,
                estimated = true,
            ),
            current.contextUsage,
        )
        assertEquals(false, current.compaction?.available)
    }

    @Test
    fun `a compaction is one timeline divider and usage reports are not activity`() {
        fun event(
            id: Long,
            type: String,
            item: JsonObject? = null,
            extra: Map<String, JsonElement> = emptyMap(),
        ) =
            RunEvent(
                id = id,
                createdAt = id * 1000,
                type = type,
                text = "",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        (item?.let { mapOf("item" to it) } ?: emptyMap()) + extra
                    ),
                runId = "run",
            )
        val started = buildJsonObject {
            put("id", "c")
            put("type", "context_compaction")
            put("status", "in_progress")
        }
        val completed = buildJsonObject {
            put("id", "c")
            put("type", "context_compaction")
            put("status", "completed")
            put("trigger", "manual")
            put("pre_tokens", 150_000)
            put("post_tokens", 9_100)
        }
        val entries =
            fixtureEntries(
                listOf(
                    RunEvent(
                        id = 1,
                        createdAt = 1000,
                        type = "chat.user",
                        text = "Première demande",
                        runId = "run",
                    ),
                    event(
                        2,
                        "context.usage",
                        extra = mapOf("usage" to buildJsonObject { put("contextTokens", 150_000) }),
                    ),
                    event(3, "item.started", started),
                    event(4, "item.completed", completed),
                ),
                chat = true,
            )
        val compactions = entries.mapNotNull { it.compaction }
        assertEquals(listOf(ContextCompaction("done", "manual", 150_000, 9_100)), compactions)
        assertEquals(1, entries.count { it.message })
        assertEquals(
            "Contexte compacté" to "150 k → 9 k jetons · Les messages précédents restent visibles",
            compactionText(compactions.single()),
        )
        val failed =
            contextCompaction(buildJsonObject { put("status", "failed") }, "item.completed", null)
        assertEquals("Échec de la compaction du contexte", compactionText(failed).first)
        assertEquals("running", contextCompaction(started, "item.started", null).state)
    }

    @Test
    fun `a compaction its coding agent failed or stopped without closing it ends`() {
        val started =
            RunEvent(
                id = 1,
                createdAt = 1000,
                type = "item.started",
                text = "",
                payload =
                    kotlinx.serialization.json.JsonObject(
                        mapOf(
                            "item" to
                                buildJsonObject {
                                    put("id", "c")
                                    put("type", "context_compaction")
                                    put("status", "in_progress")
                                }
                        )
                    ),
                runId = "run",
            )
        fun end(type: String, text: String = "", runId: String = "run") =
            RunEvent(
                id = 2,
                createdAt = 2000,
                type = type,
                text = text,
                runId = runId,
            )
        fun state(vararg events: RunEvent) =
            fixtureEntries(listOf(started) + events, chat = true).mapNotNull { it.compaction }

        // Codex fails the turn, or the run is stopped, without completing the compaction item.
        assertEquals("failed", state(end("turn.failed")).single().state)
        for (text in listOf("cancelled", "interrupted")) {
            val stopped = state(end("status", text)).single()
            assertEquals("stopped", stopped.state)
            assertEquals("Compaction du contexte interrompue" to "", compactionText(stopped))
        }
        // A later execution does not revive it; another run's failure does not end it.
        assertEquals(
            "stopped",
            state(
                    end("status", "cancelled"),
                    RunEvent(
                        id = 3,
                        createdAt = 3000,
                        type = "turn.started",
                        text = "",
                        runId = "run",
                    ),
                )
                .single()
                .state,
        )
        assertEquals("running", state(end("turn.failed", runId = "other")).single().state)
    }
}
