import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*

class ContractsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun missionsMatchRustWireAndKeepNullSeparateFromEmptySkills() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(
                            root.parentFile.parentFile.parentFile,
                            "tests/fixtures/mission-contracts.json",
                        )
                        .readText()
                )
                .jsonObject
        val wire = json.parseToJsonElement(File(root, "mission-wire.json").readText()).jsonObject
        val decoder = Json(json) { explicitNulls = false }
        for ((index, input) in corpus.getValue("creates").jsonArray.withIndex()) {
            val model = decoder.decodeFromJsonElement<CreateMissionInput>(input)
            assertEquals(wire.getValue("creates").jsonArray[index], json.encodeToJsonElement(model))
        }
        for (value in wire.getValue("views").jsonArray) {
            val model = json.decodeFromJsonElement<Mission>(value)
            assertEquals(value, json.encodeToJsonElement(model))
            assertEquals(model, json.decodeFromString<Mission>(json.encodeToString(model)))
        }
        val first = json.decodeFromJsonElement<Mission>(wire.getValue("views").jsonArray[0])
        val second = json.decodeFromJsonElement<Mission>(wire.getValue("views").jsonArray[1])
        assertNull(first.skills)
        assertEquals(emptyList(), second.skills)
        val overflow =
            JsonObject(
                wire.getValue("views").jsonArray[0].jsonObject +
                    ("nextRun" to JsonPrimitive(9007199254740992L))
            )
        assertFails { json.decodeFromJsonElement<Mission>(overflow) }
}

    @Test
    fun mcpSettingsMatchRustDefaultsAndKeepSecretChangesOutOfViews() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus = json.parseToJsonElement(File(root.parentFile.parentFile.parentFile,"tests/fixtures/mcp-contracts.json").readText()).jsonObject
        val wire = json.parseToJsonElement(File(root,"mcp-wire.json").readText()).jsonObject
        val decoder = Json { explicitNulls = false }
        corpus.getValue("creates").jsonArray.forEachIndexed { index, value ->
            val model = decoder.decodeFromJsonElement<McpInput>(value)
            assertEquals(wire.getValue("creates").jsonArray[index], json.encodeToJsonElement(model))
        }
        wire.getValue("replacements").jsonArray.forEach { value ->
            val model = json.decodeFromJsonElement<UpdateMcpInput>(value)
            assertEquals(value, json.encodeToJsonElement(model))
            for (field in listOf("name", "transport", "url", "command", "args", "auth", "clientId", "scopes", "allowPrivateNetwork", "enabled", "enabledTools")) {
                val command = value.jsonObject
                val incomplete = JsonObject(command + ("connection" to JsonObject(command.getValue("connection").jsonObject - field)))
                assertFails(field) { json.decodeFromJsonElement<UpdateMcpInput>(incomplete) }
            }
        }
        corpus.getValue("invalidCreates").jsonArray.forEach { value -> assertFails(value.toString()) { decoder.decodeFromJsonElement<McpInput>(value) } }
    }

    @Test
    fun agentsMatchRustWireAndDefaults() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(
                            root.parentFile.parentFile.parentFile,
                            "tests/fixtures/agent-contracts.json",
                        )
                        .readText()
                )
                .jsonObject
        val wire = json.parseToJsonElement(File(root, "agent-wire.json").readText()).jsonObject
        // As in the app decoder, omitted nullable fields decode to null. The wire encoder
        // is explicit: null node selections must not become the local-runner default.
        val decoder = Json(json) { explicitNulls = false }
        for ((index, input) in corpus.getValue("creates").jsonArray.withIndex()) {
            val model = decoder.decodeFromJsonElement<CreateAgentInput>(input)
            assertEquals(wire.getValue("creates").jsonArray[index], json.encodeToJsonElement(model))
        }
        for (value in wire.getValue("views").jsonArray) {
            val model = json.decodeFromJsonElement<Agent>(value)
            assertEquals(value, json.encodeToJsonElement(model))
        }
        for (value in corpus.getValue("invalidCreates").jsonArray.take(5)) {
            assertFails(value.toString()) { decoder.decodeFromJsonElement<CreateAgentInput>(value) }
        }
    }

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
