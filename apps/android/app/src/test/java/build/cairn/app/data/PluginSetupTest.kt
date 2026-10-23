package build.cairn.app.data

import java.io.File
import kotlinx.serialization.json.*
import org.junit.Assert.assertEquals
import org.junit.Test

/** The setup assistant's rules, on the corpus the installation and the web check too. */
class PluginSetupTest {
    private val corpus =
        wireJson
            .parseToJsonElement(
                File(requireNotNull(System.getProperty("cairn.pluginSetupCorpus"))).readText()
            )
            .jsonObject

    private fun manifest(element: JsonElement) =
        wireJson.decodeFromJsonElement<PluginManifest>(element)

    @Test
    fun `an older installation's manifest gives the installation's default steps`() {
        for (element in corpus.getValue("derivation").jsonArray) {
            val case = element.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            val setup = PluginEntry(manifest = manifest(case.getValue("manifest"))).resolvedSetup()
            assertEquals(name, case.getValue("effort").jsonPrimitive.contentOrNull, setup.effort)
            assertEquals(
                name,
                wireJson.decodeFromJsonElement<List<SetupStep>>(case.getValue("steps")),
                setup.steps,
            )
        }
    }

    @Test
    fun `the steps the installation resolved win, then the manifest's own`() {
        val mail = corpus.getValue("derivation").jsonArray.first().jsonObject
        val signIn = SetupStep("connect", "credential", "Sign in with Google")
        val resolved =
            PluginEntry(
                    manifest = manifest(mail.getValue("manifest")),
                    setup = PluginSetup("app", "Create an app once.", steps = listOf(signIn)),
                )
                .resolvedSetup()
        assertEquals("Create an app once.", resolved.summary)
        assertEquals(listOf("Sign in with Google"), resolved.steps.map { it.title })
        assertEquals(PluginSetup(), PluginEntry().resolvedSetup())

        // Without the resolved setup, the manifest's own steps still win over the default ones.
        val declared =
            manifest(mail.getValue("manifest"))
                .copy(setup = PluginSetup(summary = "Once.", steps = listOf(signIn)))
        val fromManifest = PluginEntry(manifest = declared).resolvedSetup()
        assertEquals("app", fromManifest.effort)
        assertEquals("Once.", fromManifest.summary)
        assertEquals(listOf("connect"), fromManifest.steps.map { it.id })
        val summaryOnly = declared.copy(setup = PluginSetup(summary = "Once."))
        assertEquals(
            wireJson.decodeFromJsonElement<List<SetupStep>>(mail.getValue("steps")),
            PluginEntry(manifest = summaryOnly).resolvedSetup().steps,
        )
        // The catalogue announces what connecting takes until the first instance exists.
        val entry = PluginEntry(id = "mail", active = "1", manifest = declared)
        assertEquals("app", entry.summary(emptyList()).effort)
        assertEquals(
            null,
            entry.summary(listOf(PluginConnection(id = "a", pluginId = "mail"))).effort,
        )
    }

    @Test
    fun `presets match and set every switch of their step`() {
        val presets = corpus.getValue("presets").jsonObject
        val step = wireJson.decodeFromJsonElement<SetupStep>(presets.getValue("step"))
        val fields = wireJson.decodeFromJsonElement<List<PluginSetting>>(presets.getValue("fields"))
        assertEquals("high", fields.last().risk)
        for (element in presets.getValue("cases").jsonArray) {
            val case = element.jsonObject
            // Android keeps switches as form text.
            val values =
                case.getValue("values").jsonObject.mapValues {
                    it.value.jsonPrimitive.boolean.toString()
                }
            assertEquals(
                values.toString(),
                case.getValue("matching").jsonPrimitive.content,
                step.matchingPreset(fields, values),
            )
        }
        for ((id, expected) in presets.getValue("applied").jsonObject) {
            assertEquals(
                id,
                expected.jsonObject.mapValues { it.value.jsonPrimitive.boolean.toString() },
                step.presetValues(fields, step.presets.single { it.id == id }),
            )
        }
    }

    @Test
    fun `each complete shared setup is offered once, named by the instances using it`() {
        val sources = corpus.getValue("sources").jsonObject
        val steps = wireJson.decodeFromJsonElement<List<SetupStep>>(sources.getValue("steps"))
        val fields = wireJson.decodeFromJsonElement<List<PluginSetting>>(sources.getValue("fields"))
        val connections =
            wireJson.decodeFromJsonElement<List<PluginConnection>>(sources.getValue("connections"))
        for (element in sources.getValue("cases").jsonArray) {
            val case = element.jsonObject
            val except = case.getValue("except").jsonPrimitive.contentOrNull
            val expected =
                case.getValue("expected").jsonArray.map { item ->
                    val source = item.jsonObject
                    SharedSource(
                        source.getValue("id").jsonPrimitive.content,
                        source.getValue("names").jsonArray.map { it.jsonPrimitive.content },
                        source.getValue("label").jsonPrimitive.content,
                    )
                }
            assertEquals(
                except.toString(),
                expected,
                sharedSources(steps, fields, connections, except),
            )
        }
        assertEquals(
            emptyList<SharedSource>(),
            sharedSources(
                listOf(SetupStep("connect", "credential", "Sign in")),
                emptyList(),
                connections,
            ),
        )
    }

    @Test
    fun `long identifiers are shortened around an ellipsis`() {
        assertEquals(
            "481203…nt.com",
            shortValue("481203947512-k2d9qv0bm3h1.apps.googleusercontent.com", 6),
        )
        assertEquals("short", shortValue("short"))
    }
}
