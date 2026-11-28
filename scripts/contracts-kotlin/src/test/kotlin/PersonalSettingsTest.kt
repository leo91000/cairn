import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.json.*

class PersonalSettingsTest {
    private val json = Json { encodeDefaults = true }

    private inline fun <reified T> roundTrip(values: JsonElement) =
        values.jsonArray.map {
            val decoded = json.decodeFromJsonElement<T>(it)
            assertEquals(it, json.encodeToJsonElement(decoded))
            decoded
        }

    @Test
    fun personalStateAndSettingsRetainTheirWireMeaning() {
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/personal-settings-contracts.json")
                        .readText()
                )
                .jsonObject
        val marks = roundTrip<MarkReadInput>(corpus.getValue("marks"))
        assertIs<ReadTarget>(marks[0].items[0].target)
        assertIs<ReadTarget>(marks[0].items[1].target)
        val snoozes = roundTrip<SetConversationSnoozeInput>(corpus.getValue("snoozes"))
        assertIs<ConversationSnoozeAction.B>(snoozes[1].action)
        assertEquals(1761886400000L, (snoozes[0].action as ConversationSnoozeAction.A).until)
        roundTrip<SetConversationSnoozeOutput>(corpus.getValue("snoozeOutputs"))
        roundTrip<InstallationSettings>(corpus.getValue("settings"))
        roundTrip<InstallationOverview>(corpus.getValue("overviews"))
        roundTrip<ListAuditOutput>(corpus.getValue("audits"))
        corpus.getValue("invalidSnoozes").jsonArray.forEach {
            assertFails(it.toString()) {
                json.decodeFromJsonElement<SetConversationSnoozeInput>(it)
            }
        }
        // Kotlin enforces bounds and patterns its templates support; the server
        // remains authoritative for list sizes.
        corpus.getValue("invalidMarks").jsonArray.drop(1).forEach {
            assertFails(it.toString()) { json.decodeFromJsonElement<MarkReadInput>(it) }
        }
    }
}
