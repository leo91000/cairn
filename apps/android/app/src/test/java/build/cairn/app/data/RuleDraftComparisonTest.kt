package build.cairn.app.data

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RuleDraftComparisonTest {
    @Test
    fun optionalTextClearsEmptyInputsAndPreservesMeaningfulValues() {
        assertNull(optionalRuleText(templateValue()))
        assertNull(optionalRuleText(literalRuleValue(JsonPrimitive(""))))
        assertNull(optionalRuleText(literalRuleValue(JsonNull)))
        for (value in
            listOf(
                templateValue(" "),
                literalRuleValue(JsonPrimitive("alice")),
                literalRuleValue(JsonPrimitive(" ")),
                literalRuleValue(JsonPrimitive(0)),
            )) {
            assertEquals(value, optionalRuleText(value))
        }
    }

    @Test
    fun optionalNullsMatchButLiteralNullPropertiesRemainData() {
        val original =
            wireJson
                .parseToJsonElement(
                    """{"name":"Draft","steps":[{"type":"action","id":"save","action":{"type":"storeValue","scope":"rule","name":"last","value":{"type":"literal","value":{"missing":null}}}}]}"""
                )
                .jsonObject
        val optional =
            wireJson
                .parseToJsonElement(
                    original
                        .toString()
                        .replace("\"scope\":\"rule\"", "\"scope\":\"rule\",\"key\":null")
                )
                .jsonObject
        assertTrue(sameRuleDefinition(original, optional))
        val emptyLabel =
            JsonObject(
                optional +
                    ("steps" to
                        JsonArray(
                            optional.list("steps").map {
                                JsonObject(it.jsonObject + ("label" to JsonPrimitive("")))
                            }
                        ))
            )
        assertTrue(sameRuleDefinition(original, emptyLabel))
        val changed =
            wireJson
                .parseToJsonElement(original.toString().replace("{\"missing\":null}", "{}"))
                .jsonObject
        assertFalse(sameRuleDefinition(original, changed))
    }
}
