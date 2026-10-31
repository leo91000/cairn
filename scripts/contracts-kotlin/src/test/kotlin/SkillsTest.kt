import build.cairn.contracts.*
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFails
import kotlinx.serialization.json.*

class SkillsTest {
    private val json = Json {
        encodeDefaults = true
        explicitNulls = true
    }

    @Test
    fun skillCommandsUseTheSameDiscriminatorsAndEnvelopeAsRustAndWeb() {
        val corpus =
            json
                .parseToJsonElement(File("../../tests/fixtures/skill-contracts.json").readText())
                .jsonObject
        for (value in corpus.getValue("imports").jsonArray) {
            val decoded = json.decodeFromJsonElement<ImportSkillInput>(value)
            val wire = json.encodeToJsonElement(decoded).jsonObject
            assertEquals(value.jsonObject.getValue("source"), wire.getValue("source"))
            assertEquals(decoded, json.decodeFromJsonElement<ImportSkillInput>(wire))
        }
        for (value in corpus.getValue("changes").jsonArray) {
            val decoded = json.decodeFromJsonElement<SkillChange>(value)
            val wire = json.encodeToJsonElement(decoded).jsonObject
            assertEquals(value.jsonObject.getValue("op"), wire.getValue("op"))
            assertEquals(decoded, json.decodeFromJsonElement<SkillChange>(wire))
        }
        for (value in corpus.getValue("invalidImports").jsonArray) assertFails {
            json.decodeFromJsonElement<ImportSkillInput>(value)
        }
        for (value in corpus.getValue("invalidChanges").jsonArray) assertFails {
            json.decodeFromJsonElement<SkillChange>(value)
        }
        val bundle = json.decodeFromJsonElement<SkillBundle>(corpus.getValue("bundle"))
        assertEquals(corpus.getValue("bundle"), json.encodeToJsonElement(bundle))
    }
}
