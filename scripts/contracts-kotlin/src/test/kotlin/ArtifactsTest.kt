import build.cairn.contracts.*
import java.io.File
import kotlin.test.*
import kotlinx.serialization.json.*

class ArtifactsTest {
    private val json = Json { encodeDefaults = true }

    @Test
    fun artifactCommandsKeepStringVisibilityAndVersionMetadata() {
        val corpus =
            json
                .parseToJsonElement(File("../../tests/fixtures/artifact-contracts.json").readText())
                .jsonObject
        for (value in corpus.getValue("visibilityInputs").jsonArray) {
            val decoded = json.decodeFromJsonElement<SetRunArtifactVisibilityInput>(value)
            assertEquals(value, json.encodeToJsonElement(decoded))
        }
        for (value in corpus.getValue("invalidVisibilityInputs").jsonArray) {
            assertFails { json.decodeFromJsonElement<SetRunArtifactVisibilityInput>(value) }
        }
        val metadata = json.decodeFromJsonElement<Deliverable>(corpus.getValue("metadata"))
        assertEquals(2L, metadata.version)
        assertEquals(262144L, metadata.size)
        assertEquals("a".repeat(64), metadata.digest)
        assertEquals(
            metadata,
            json.decodeFromJsonElement<Deliverable>(json.encodeToJsonElement(metadata)),
        )
    }
}
