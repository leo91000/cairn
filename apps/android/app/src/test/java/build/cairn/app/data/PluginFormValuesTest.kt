package build.cairn.app.data

import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test

class PluginFormValuesTest {
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
            val instanceSecrets =
                (scenario["instanceSecretsInput"] ?: scenario.getValue("secrets"))
                    .jsonObject
                    .mapValues { it.value.jsonPrimitive.content }
            assertEquals(name, scenario["instanceSecrets"], pluginSecrets(instanceSecrets))
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
