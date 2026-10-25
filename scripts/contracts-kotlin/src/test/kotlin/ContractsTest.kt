import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*

class ContractsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun accountMethodsCorpusDecodesAppResponses() {
        val corpus =
            json
                .parseToJsonElement(
                    File(System.getProperty("cairn.accountMethodsFixtures")).readText()
                )
                .jsonObject
        for (value in corpus.getValue("valid").jsonArray) {
            val decoded = json.decodeFromJsonElement<ListAccountMethodsOutput>(value)
            assertEquals(value, json.encodeToJsonElement(decoded))
        }
        for (value in corpus.getValue("invalid").jsonArray) {
            assertFails(value.toString()) {
                json.decodeFromJsonElement<ListAccountMethodsOutput>(value)
            }
        }
    }

    @Test
    fun projectsMatchRustWireAndDefaults() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(
                            root.parentFile.parentFile.parentFile,
                            "tests/fixtures/project-contracts.json",
                        )
                        .readText()
                )
                .jsonObject
        val wire = json.parseToJsonElement(File(root, "project-wire.json").readText()).jsonObject
        for ((index, input) in corpus.getValue("creates").jsonArray.withIndex()) {
            val model = json.decodeFromJsonElement<CreateProjectInput>(input)
            assertEquals(wire.getValue("creates").jsonArray[index], json.encodeToJsonElement(model))
        }
        for (value in wire.getValue("views").jsonArray) {
            val project = json.decodeFromJsonElement<Project>(value)
            assertEquals(value, json.encodeToJsonElement(project))
            assertEquals(project, json.decodeFromString<Project>(json.encodeToString(project)))
        }
    }

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
