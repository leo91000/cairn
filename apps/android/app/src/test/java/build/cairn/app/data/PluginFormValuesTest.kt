package build.cairn.app.data

import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test

class PluginFormValuesTest {
    @Test
    fun `client whitespace corpus matches actual Kotlin runtime and production helpers`() {
        val corpusFile = File(requireNotNull(System.getProperty("cairn.pluginFormCorpus")))
        val boundaries =
            wireJson
                .parseToJsonElement(
                    File(corpusFile.parentFile, "plugin-form-whitespace.json").readText()
                )
                .jsonObject
        val blankCodes =
            boundaries
                .getValue("blank")
                .jsonArray
                .map { it.jsonPrimitive.content.toInt(16) }
                .toSet()

        // Exhaust the Char domain: Kotlin uses Character.isWhitespace OR isSpaceChar.
        // This also checks that the shared list has not missed a whitespace member.
        for (code in Char.MIN_VALUE.code..Char.MAX_VALUE.code) {
            val value = code.toChar().toString()
            assertEquals("blank U+${code.toString(16)}", code in blankCodes, value.isBlank())
            assertEquals(
                "trim U+${code.toString(16)}",
                if (code in blankCodes) "" else value,
                value.trim(),
            )
        }

        val fields = listOf(PluginSetting(key = "note", type = "text"))
        for ((group, codes) in boundaries) {
            for (element in codes.jsonArray) {
                val code = element.jsonPrimitive.content
                val character = String(Character.toChars(code.toInt(16)))
                for (value in
                    listOf(
                        character,
                        "${character}x",
                        "x${character}",
                        "${character}x${character}",
                    )) {
                    val normalized = if (group == "blank") value.replace(character, "") else value
                    val expected =
                        if (normalized.isEmpty()) JsonObject(emptyMap())
                        else buildJsonObject { put("note", normalized) }
                    val label = "U+$code"
                    assertEquals(label, normalized.isEmpty(), value.isBlank())
                    assertEquals(label, normalized, value.trim())
                    val opened = fields.associate { it.key to formValue(it, JsonPrimitive(value)) }
                    assertEquals(label, expected, pluginSettings(fields, opened))
                    val edited =
                        fields.associate { it.key to formValue(it, null) } + ("note" to value)
                    assertEquals(label, expected, pluginSettings(fields, edited))
                    assertEquals(label, expected, pluginSecrets(mapOf("note" to value)))
                    val raw =
                        if (normalized.isEmpty()) JsonObject(emptyMap())
                        else buildJsonObject { put("note", value) }
                    assertEquals(label, raw, pluginSecrets(mapOf("note" to value), trim = false))
                }

                val choices =
                    listOf(PluginSetting(key = "choice", type = "select", required = true))
                assertEquals(
                    buildJsonObject { put("choice", character) },
                    pluginSettings(choices, mapOf("choice" to character)),
                )
                assertEquals(JsonObject(emptyMap()), pluginSettings(choices, mapOf("choice" to "")))
            }
        }
    }

    @Test
    fun `web and Android submit the same shared form scenarios`() {
        val corpus =
            wireJson
                .parseToJsonElement(
                    File(requireNotNull(System.getProperty("cairn.pluginFormCorpus"))).readText()
                )
                .jsonArray
        for (element in corpus) {
            val scenario = element.jsonObject
            val name = scenario.getValue("name").jsonPrimitive.content
            val fields =
                wireJson.decodeFromJsonElement<List<PluginSetting>>(scenario.getValue("fields"))
            val initial = scenario.getValue("initial").jsonObject
            val opened = fields.associate { it.key to formValue(it, initial[it.key]) }
            assertEquals(name, scenario["opened"], pluginSettings(fields, opened))
            val edits =
                scenario.getValue("edits").jsonObject.mapValues { (key, value) ->
                    formValue(fields.find { it.key == key } ?: PluginSetting(key = key), value)
                }
            val settings = pluginSettings(fields, opened + edits)
            assertEquals(name, scenario["settings"], settings)
            val secrets =
                scenario.getValue("secrets").jsonObject.mapValues { it.value.jsonPrimitive.content }
            assertEquals(name, scenario["instanceSecrets"], pluginSecrets(secrets))
            val expectedAction =
                JsonObject(
                    scenario.getValue("settings").jsonObject +
                        scenario.getValue("actionSecrets").jsonObject
                )
            assertEquals(
                name,
                expectedAction,
                JsonObject(settings + pluginSecrets(secrets, trim = false)),
            )
        }
    }
}
