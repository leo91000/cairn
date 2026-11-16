package build.cairn.app.data

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RuleSchemaTest {
    @Test
    fun composedToolInputsExposeFieldsAndOnlyCommonRequirements() {
        val root =
            wireJson
                .parseToJsonElement(
                    """{"type":"object","allOf":[{"required":["text"],"properties":{"text":{"type":"string"}}}],"oneOf":[{"required":["count"],"properties":{"count":{"type":"number"},"shared":{"type":"number"}}},{"required":["flag"],"properties":{"flag":{"type":"boolean"},"shared":{"type":"string"}}}]}"""
                )
                .jsonObject
        val indexed = ruleToolInputSchema(root)
        assertEquals(setOf("text", "count", "shared", "flag"), indexed.obj("properties")!!.keys)
        assertEquals(listOf(JsonPrimitive("text")), indexed.list("required"))
        assertTrue(ruleSchemaUsesJson(indexed.obj("properties").obj("shared")!!))
        val recursive =
            wireJson
                .parseToJsonElement(
                    """{"type":"object","${'$'}ref":"#/${'$'}defs/Loop","${'$'}defs":{"Loop":{"allOf":[{"${'$'}ref":"#/${'$'}defs/Loop"}]}}}"""
                )
                .jsonObject
        assertTrue(ruleToolInputSchema(recursive).obj("properties")!!.isEmpty())
    }

    @Test
    fun nullableReferencesKeepTheirMeaningAfterResolution() {
        val schema =
            wireJson
                .parseToJsonElement(
                    """{"type":"object","required":["notify"],"properties":{"notify":{"${'$'}ref":"#/${'$'}defs/flag"}},"${'$'}defs":{"flag":{"type":["boolean","null"],"default":null}}}"""
                )
                .jsonObject
        val resolved = resolveRuleSchema(schema)
        val flag = resolved.obj("properties").obj("notify")!!
        assertEquals("boolean", flag.text("type"))
        assertTrue(ruleSchemaAllowsNull(flag))
        assertEquals(JsonNull, defaultRuleLiteral(flag))
        assertEquals(JsonPrimitive(false), concreteRuleLiteralDefault(flag))
        assertEquals(
            wireJson.parseToJsonElement("""{"notify":null}"""),
            defaultRuleLiteral(resolved),
        )
    }

    @Test
    fun nullableAlternativesAndMixedTypesKeepDistinctControls() {
        val nullable =
            resolveRuleSchema(
                wireJson
                    .parseToJsonElement(
                        """{"anyOf":[{"type":"null"},{"type":"boolean"}],"default":null}"""
                    )
                    .jsonObject
            )
        assertEquals("boolean", nullable.text("type"))
        assertTrue(ruleSchemaAllowsNull(nullable))
        assertFalse(ruleSchemaUsesJson(nullable))
        assertEquals(JsonPrimitive(false), concreteRuleLiteralDefault(nullable))
        assertFalse(
            ruleSchemaAllowsNull(
                wireJson
                    .parseToJsonElement("""{"type":["boolean","null"],"enum":[true,false]}""")
                    .jsonObject
            )
        )
        assertFalse(
            ruleSchemaAllowsNull(
                wireJson
                    .parseToJsonElement("""{"oneOf":[{"type":"null"},{"type":"null"}]}""")
                    .jsonObject
            )
        )
        assertNull(
            concreteRuleLiteralDefault(
                wireJson.parseToJsonElement("""{"type":"null"}""").jsonObject
            )
        )
        assertTrue(
            ruleSchemaUsesJson(
                resolveRuleSchema(
                    wireJson
                        .parseToJsonElement("""{"type":["object","string","null"]}""")
                        .jsonObject
                )
            )
        )
    }

    @Test
    fun recursiveAlternativesResolveToABoundedFallback() {
        val reference = JsonObject(mapOf("${'$'}ref" to JsonPrimitive("#/${'$'}defs/loop")))
        val alternatives = JsonObject(mapOf("anyOf" to JsonArray(List(8) { reference })))
        val definitions = JsonObject(mapOf("loop" to alternatives))
        val schema =
            JsonObject(
                mapOf(
                    "${'$'}defs" to definitions,
                    "${'$'}ref" to JsonPrimitive("#/${'$'}defs/loop"),
                )
            )
        assertTrue(resolveRuleSchema(schema).toString().length < 10000)
    }
}
