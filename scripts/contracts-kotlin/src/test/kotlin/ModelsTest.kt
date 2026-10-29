import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.json.*

class ModelsTest {
    @Test
    fun sharedModelCorpusPreservesRoutesSpeedsAndUnknownCapabilities() {
        val json = Json { encodeDefaults = true }
        val root = File(System.getProperty("cairn.contractWire")).parentFile
        val corpus =
            json
                .parseToJsonElement(
                    File(root, "../../../tests/fixtures/model-contracts.json").readText()
                )
                .jsonObject
        val catalogs =
            corpus.getValue("catalogs").jsonArray.map {
                json.decodeFromJsonElement<ModelCatalog>(it)
            }
        for (catalog in catalogs) assertEquals(
            catalog,
            json.decodeFromJsonElement<ModelCatalog>(json.encodeToJsonElement(catalog)),
        )
        assertEquals(true, catalogs[1].models.first().serviceTiers!!.first().limited)
        assertEquals(listOf("credits", "key"), catalogs[2].models.first().routes.map { it.billing })
        assertEquals(null, catalogs[2].models.first().serviceTiers)
        assertEquals(emptyList(), catalogs[3].models.first().serviceTiers)
        for (input in corpus.getValue("inputs").jsonArray) json.decodeFromJsonElement<
            ListModelsInput
        >(
            input
        )
        for (input in corpus.getValue("invalidInputs").jsonArray) assertFails {
            json.decodeFromJsonElement<ListModelsInput>(input)
        }
    }
}
