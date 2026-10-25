import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*

class ContractsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun qualificationCorpusDecodesAndRoundTrips() {
        val corpus =
            json
                .parseToJsonElement(File(System.getProperty("cairn.contractFixtures")).readText())
                .jsonObject
        val valid =
            corpus.getValue("valid").jsonArray.map {
                json.decodeFromJsonElement<QualificationFixture>(it)
            }
        val expectedWire =
            json
                .parseToJsonElement(File(System.getProperty("cairn.contractWire")).readText())
                .jsonArray
        for ((model, expected) in valid.zip(expectedWire)) {
            val actual = json.encodeToJsonElement(model)
            assertEquals(expected, actual)
            assertTrue(actual.jsonObject.containsKey("nullable"))
            assertEquals(
                expected.jsonObject.containsKey("omitted"),
                actual.jsonObject.containsKey("omitted"),
            )
        }
        val missingNullable = JsonObject(expectedWire[0].jsonObject - "nullable")
        assertFails { json.decodeFromJsonElement<QualificationFixture>(missingNullable) }
        assertFalse(valid[0].enabled)
        assertNull(valid[0].nullable)
        assertNull(valid[0].omitted)
        assertEquals("mission", valid[0].names.value["run"])
        assertEquals(9007199254740991L, valid[0].count)
        assertTrue(valid[0].result is QualificationResult.A)
        assertEquals(
            "Run already active",
            (valid[1].result as QualificationResult.B).response.error,
        )
        for (value in valid) {
            val encoded = json.encodeToString(value)
            assertEquals(value, json.decodeFromString<QualificationFixture>(encoded))
        }
        assertFalse(json.encodeToJsonElement(valid[0]).jsonObject.containsKey("omitted"))
        for (value in corpus.getValue("invalid").jsonArray) {
            assertFails(value.toString()) {
                json.decodeFromJsonElement<QualificationFixture>(value)
            }
        }
        val input = json.decodeFromString<ResumeRunInput>("""{"runId":"${valid[0].id}"}""")
        assertEquals(
            input.runId,
            json
                .decodeFromString<ResumeRunOutput>(
                    json.encodeToString(ResumeRunOutput(input.runId))
                )
                .runId,
        )
    }
}
