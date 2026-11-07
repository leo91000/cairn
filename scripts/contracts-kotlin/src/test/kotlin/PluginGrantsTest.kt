import build.cairn.contracts.ReplaceAgentPluginGrantsInput
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlinx.serialization.json.*

class PluginGrantsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun fullAndSelectedGrantsPreserveTheirDiscriminatorsResourcesAndCredentialChoice() {
        val wire =
            json.parseToJsonElement(
                """
                {
                    "agentId":"00000000-0000-4000-8000-000000000010",
                    "revision":"r1",
                    "grants":[
                        {"instanceId":"00000000-0000-4000-8000-000000000011","access":{"type":"full"}},
                        {"instanceId":"00000000-0000-4000-8000-000000000012","access":{"type":"selected","contributions":[
                            {"kind":"hook","name":"message","resources":[{"resourceType":"channel","id":"support"}]},
                            {"kind":"runCredentials","name":"","resources":[]}
                        ]}}
                    ],
                    "runCredentialInstances":["00000000-0000-4000-8000-000000000012"]
                }
                """
                    .trimIndent()
            )
        val decoded = json.decodeFromJsonElement<ReplaceAgentPluginGrantsInput>(wire)
        assertEquals(wire, json.encodeToJsonElement(decoded))
        val invalid = wire.toString().replace("\"hook\"", "\"unknown\"")
        assertFails { json.decodeFromString<ReplaceAgentPluginGrantsInput>(invalid) }
    }
}
