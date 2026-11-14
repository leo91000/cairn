package build.cairn.app.data

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class RuleDraftComparisonTest {
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
        val changed =
            wireJson
                .parseToJsonElement(original.toString().replace("{\"missing\":null}", "{}"))
                .jsonObject
        assertFalse(sameRuleDefinition(original, changed))
    }
}
