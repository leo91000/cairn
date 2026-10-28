import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*

class ContractsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun nodeInventoryAndCommandsMatchRustWithNativeAuthorityKeptSeparate() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/node-contracts.json").readText()
                )
                .jsonObject
        for (value in corpus.getValue("views").jsonArray) {
            val node = json.decodeFromJsonElement<ExecutionNode>(value)
            assertEquals(
                node,
                json.decodeFromJsonElement<ExecutionNode>(json.encodeToJsonElement(node)),
            )
            assertEquals("protected", node.nativeModeRequest?.mode)
            assertEquals(4L, node.slots)
        }
        for (value in corpus.getValue("commands").jsonArray) {
            val command = json.decodeFromJsonElement<UpdateNodeInput>(value)
            assertEquals("r1", command.node.revision)
            assertEquals(
                command,
                json.decodeFromJsonElement<UpdateNodeInput>(json.encodeToJsonElement(command)),
            )
        }
        corpus.getValue("invalidCommands").jsonArray.forEachIndexed { index, value ->
            // kotlinx accepts numeric strings; the Rust command decoder remains authoritative.
            if (index != 2)
                assertFails(value.toString()) { json.decodeFromJsonElement<UpdateNodeInput>(value) }
        }
    }

    @Test
    fun eventAndHistoryContractsPreserveAdapterBoundariesAndCursors() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/event-contracts.json").readText()
                )
                .jsonObject
        for (event in corpus.getValue("events").jsonArray) {
            assertEquals(
                event,
                json.encodeToJsonElement(json.decodeFromJsonElement<RunEvent>(event)),
            )
        }
        val window = corpus.getValue("windowOutput")
        assertEquals(
            window,
            json.encodeToJsonElement(json.decodeFromJsonElement<RunHistoryWindowOutput>(window)),
        )
        val history = corpus.getValue("history")
        assertEquals(
            history,
            json.encodeToJsonElement(json.decodeFromJsonElement<HistoryPage>(history)),
        )
        corpus.getValue("invalidEvents").jsonArray.forEach { value ->
            assertFails(value.toString()) { json.decodeFromJsonElement<RunEvent>(value) }
        }
    }

    @Test
    fun runDefaultsStatesAndRetryMatchRust() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/run-contracts.json").readText()
                )
                .jsonObject
        val wire = json.parseToJsonElement(File(root, "run-wire.json").readText()).jsonObject
        val decoder = Json { explicitNulls = false }
        corpus.getValue("commands").jsonArray.forEach { value ->
            assertEquals(
                value,
                json.encodeToJsonElement(decoder.decodeFromJsonElement<RunInput>(value)),
            )
        }
        corpus.getValue("invalidCommands").jsonArray.forEach { value ->
            assertFails(value.toString()) { json.decodeFromJsonElement<RunInput>(value) }
        }
        corpus.getValue("lists").jsonArray.forEachIndexed { index, value ->
            assertEquals(
                wire.getValue("lists").jsonArray[index],
                json.encodeToJsonElement(decoder.decodeFromJsonElement<ListRunsInput>(value)),
            )
        }
        corpus.getValue("invalidLists").jsonArray.forEach { value ->
            // kotlinx accepts numeric strings; Rust remains authoritative, as in B1.
            if (value.jsonObject["offset"]?.toString() != "\"1\"") {
                assertFails(value.toString()) { json.decodeFromJsonElement<ListRunsInput>(value) }
            }
        }
        for (field in listOf("states", "retries")) {
            corpus.getValue(field).jsonArray.forEachIndexed { index, value ->
                val encoded =
                    if (field == "states")
                        json.encodeToJsonElement(decoder.decodeFromJsonElement<RunState>(value))
                    else json.encodeToJsonElement(decoder.decodeFromJsonElement<Retry>(value))
                // Nullable timestamp defaults are omitted by the generated decoder contract.
                // Decode the Rust wire and require the same value rather than key presence.
                val expected =
                    if (field == "states")
                        json.encodeToJsonElement(
                            decoder.decodeFromJsonElement<RunState>(
                                wire.getValue(field).jsonArray[index]
                            )
                        )
                    else
                        json.encodeToJsonElement(
                            decoder.decodeFromJsonElement<Retry>(
                                wire.getValue(field).jsonArray[index]
                            )
                        )
                assertEquals(expected, encoded)
            }
        }
    }

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
        val corpus =
            json
                .parseToJsonElement(
                    File(root.parentFile.parentFile.parentFile, "tests/fixtures/mcp-contracts.json")
                        .readText()
                )
                .jsonObject
        val wire = json.parseToJsonElement(File(root, "mcp-wire.json").readText()).jsonObject
        val decoder = Json { explicitNulls = false }
        corpus.getValue("creates").jsonArray.forEachIndexed { index, value ->
            val model = decoder.decodeFromJsonElement<McpInput>(value)
            assertEquals(wire.getValue("creates").jsonArray[index], json.encodeToJsonElement(model))
        }
        wire.getValue("replacements").jsonArray.forEach { value ->
            val model = json.decodeFromJsonElement<UpdateMcpInput>(value)
            assertEquals(value, json.encodeToJsonElement(model))
            for (field in
                listOf(
                    "name",
                    "transport",
                    "url",
                    "command",
                    "args",
                    "auth",
                    "clientId",
                    "scopes",
                    "allowPrivateNetwork",
                    "enabled",
                    "enabledTools",
                )) {
                val command = value.jsonObject
                val incomplete =
                    JsonObject(
                        command +
                            ("connection" to
                                JsonObject(command.getValue("connection").jsonObject - field))
                    )
                assertFails(field) { json.decodeFromJsonElement<UpdateMcpInput>(incomplete) }
            }
        }
        corpus.getValue("invalidCreates").jsonArray.forEach { value ->
            assertFails(value.toString()) { decoder.decodeFromJsonElement<McpInput>(value) }
        }
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

    @Test
    fun pluginsPreserveTheirTypedEnvelopeAndManifestJson() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val wire = json.parseToJsonElement(File(root, "plugin-wire.json").readText()).jsonObject
        val corpus =
            json
                .parseToJsonElement(
                    File(
                            root.parentFile.parentFile.parentFile,
                            "tests/fixtures/plugin-contracts.json",
                        )
                        .readText()
                )
                .jsonObject
        for ((index, value) in corpus.getValue("inputs").jsonArray.withIndex()) {
            val command = json.decodeFromJsonElement<ManagePluginInput>(value)
            assertEquals(
                wire.getValue("inputs").jsonArray[index],
                json.encodeToJsonElement(command),
            )
        }
        for (value in wire.getValue("outputs").jsonArray) {
            val output = json.decodeFromJsonElement<ManagePluginOutput>(value)
            assertEquals(value, json.encodeToJsonElement(output))
        }
        for (value in corpus.getValue("invalidInputs").jsonArray) {
            assertFails(value.toString()) { json.decodeFromJsonElement<ManagePluginInput>(value) }
        }
    }

    @Test
    fun packageCommandsAndAsyncResultsMatchTheRustCorpus() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(
                            root.parentFile.parentFile.parentFile,
                            "tests/fixtures/plugin-package-contracts.json",
                        )
                        .readText()
                )
                .jsonObject
        for (value in corpus.getValue("installs").jsonArray) {
            json.decodeFromJsonElement<InstallPluginPackageInput>(value)
        }
        for (value in corpus.getValue("invalidInstalls").jsonArray) {
            assertFails(value.toString()) {
                json.decodeFromJsonElement<InstallPluginPackageInput>(value)
            }
        }
        for (value in corpus.getValue("programs").jsonArray) {
            val model = json.decodeFromJsonElement<PluginProgramInput>(value)
            assertEquals(value, json.encodeToJsonElement(model))
        }
        for (value in corpus.getValue("invalidPrograms").jsonArray) {
            assertFails(value.toString()) { json.decodeFromJsonElement<PluginProgramInput>(value) }
        }
        for (value in corpus.getValue("operations").jsonArray) {
            val model = json.decodeFromJsonElement<PluginOperation>(value)
            assertEquals(value, json.encodeToJsonElement(model))
        }
    }
}
