package build.cairn.app.data

import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PluginPackagesTest {
    @Test
    fun preparesTheSameTomlAndModulesAsWebUploads() {
        val input =
            pluginPackageInput(
                mapOf(
                    "manifest.toml" to "id = 'fixture'",
                    "main.luau" to "return {}",
                    "lib/rest.luau" to "return {}",
                    "README.md" to "ignored",
                )
            )
        assertEquals("toml", input.manifestFormat)
        assertEquals(JsonPrimitive("id = 'fixture'"), input.manifest)
        assertEquals(mapOf("lib/rest.luau" to "return {}"), input.modules.value)
        assertEquals("return {}", input.source)
    }

    @Test
    fun refusesAmbiguousOrIncompletePackagesBeforeUpload() {
        for (files in
            listOf(
                mapOf("manifest.json" to "{}"),
                mapOf("manifest.json" to "{}", "manifest.toml" to "", "main.luau" to "return {}"),
            )) {
            assertThrows(IllegalArgumentException::class.java) { pluginPackageInput(files) }
        }
    }

    @Test
    fun boundsASelectedFileBeforeReadingTheWholeStream() {
        val input = java.io.ByteArrayInputStream(ByteArray(458_753))
        assertThrows(IllegalArgumentException::class.java) { readPluginFile(input) }
    }
}
