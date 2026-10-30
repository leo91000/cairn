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
        assertIs<ReadTarget.A>(marks[0].items[0].target)
        assertIs<ReadTarget.B>(marks[0].items[1].target)
        val asides = roundTrip<SetOutcomeAsideInput>(corpus.getValue("asides"))
        assertIs<OutcomeAsideAction.C>(asides[2].action)
        assertEquals(1761886400000L, (asides[1].action as OutcomeAsideAction.B).until)
        roundTrip<SetOutcomeAsideOutput>(corpus.getValue("asideOutputs"))
        roundTrip<InstallationSettings>(corpus.getValue("settings"))
        roundTrip<InstallationOverview>(corpus.getValue("overviews"))
        roundTrip<ListAuditOutput>(corpus.getValue("audits"))
        roundTrip<StorageSettings>(corpus.getValue("storageViews"))
        corpus.getValue("storageInputs").jsonArray.forEach {
            json.decodeFromJsonElement<SaveStorageSettingsInput>(it)
        }
        corpus.getValue("invalidAsides").jsonArray.forEach {
            assertFails(it.toString()) { json.decodeFromJsonElement<SetOutcomeAsideInput>(it) }
        }
        // Kotlin enforces bounds and patterns its templates support; the server
        // remains authoritative for list sizes.
        corpus.getValue("invalidMarks").jsonArray.drop(1).forEach {
            assertFails(it.toString()) { json.decodeFromJsonElement<MarkReadInput>(it) }
        }
        corpus.getValue("invalidStorageInputs").jsonArray.take(3).forEach {
            assertFails(it.toString()) {
                json.decodeFromJsonElement<SaveStorageSettingsInput>(it)
            }
        }
    }
}
