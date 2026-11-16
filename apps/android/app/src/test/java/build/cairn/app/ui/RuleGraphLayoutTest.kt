package build.cairn.app.ui

import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test

class RuleGraphLayoutTest {
    @Test
    fun oneSidedNestingDoesNotDoubleTheEmptyBranchAtEveryLevel() {
        var nodes: List<JsonElement> = emptyList()
        repeat(7) { depth ->
            nodes =
                listOf(
                    ruleObject(
                        "type" to JsonPrimitive("if"),
                        "then" to JsonArray(nodes),
                        "otherwise" to JsonArray(emptyList()),
                    )
                )
            assertEquals(240f + (depth + 1) * 304f, ruleFlowWidth(nodes))
        }
    }

    @Test
    fun loopsReserveASeparateReturnAndExitLane() {
        assertEquals(
            336f,
            ruleFlowWidth(
                listOf(
                    ruleObject("type" to JsonPrimitive("forEach"), "body" to JsonArray(emptyList()))
                )
            ),
        )
    }
}
