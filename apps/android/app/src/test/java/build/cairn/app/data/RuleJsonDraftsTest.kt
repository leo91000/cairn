package build.cairn.app.data

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RuleJsonDraftsTest {
    @Test
    fun jsonSyntaxRequiresQuotedText() {
        for (raw in
            listOf(
                "plain",
                "01",
                "[plain]",
                "{\"x\":plain}",
                "NaN",
                "+1",
                "1.",
                "1e999",
                "{\"x\":1e999}",
            )) {
            val parsed = runCatching { parseRuleJsonInput(raw) }
            assertTrue(
                "Unexpected JSON acceptance for $raw: ${parsed.getOrNull()}",
                parsed.isFailure,
            )
        }
    }

    @Test
    fun acceptsEveryJsonValueKind() {
        for (raw in
            listOf(
                "null",
                "true",
                "false",
                "-1.25e+2",
                "\"plain\"",
                "[]",
                "{\"x\":[1,\"text\",null]}",
            )) {
            assertEquals(wireJson.parseToJsonElement(raw), parseRuleJsonInput(raw))
        }
    }

    @Test
    fun invalidIntentSurvivesRecoveryWithoutInterpretingLiteralData() {
        val invalid =
            literalWithJsonDrafts(JsonPrimitive("{\"count\":"), jsonInputDraft("{\"count\":"))
        val restored = wireJson.parseToJsonElement(invalid.toString())
        for (expects in listOf("json", "number", "boolean", "text")) assertEquals(
            invalid,
            typedComparisonRight(restored, expects, listOnly = false),
        )
        assertEquals(1, ruleJsonDraftProblems(restored).size)
        val lookalike =
            wireJson.parseToJsonElement(
                """{"type":"literal","_jsonDrafts":{"":{"text":"data","message":"also data"}}}"""
            )
        assertTrue(ruleJsonDraftProblems(literalWithJsonDrafts(lookalike)).isEmpty())
        assertTrue(ruleJsonDraftProblems(literalRuleValue(JsonPrimitive("{\"count\":"))).isEmpty())
    }

    @Test
    fun nestedDraftErrorsFollowRemovedArrayIndexes() {
        var drafts =
            replaceChildJsonDrafts(
                JsonObject(emptyMap()),
                1,
                replaceChildJsonDrafts(JsonObject(emptyMap()), "a/b~c", jsonInputDraft("")),
            )
        drafts = replaceChildJsonDrafts(drafts, 2, jsonInputDraft("["))
        drafts = removeJsonDraftItem(drafts, 0)
        assertEquals(jsonInputDraft(""), childJsonDrafts(childJsonDrafts(drafts, 0), "a/b~c"))
        assertEquals(jsonInputDraft("["), childJsonDrafts(drafts, 1))
        drafts = replaceChildJsonDrafts(drafts, 0, JsonObject(emptyMap()))
        assertTrue(childJsonDrafts(drafts, 0).isEmpty())
        assertEquals(jsonInputDraft("["), childJsonDrafts(drafts, 1))
        assertTrue(removeJsonDraftItem(drafts, 1).isEmpty())
    }
}
