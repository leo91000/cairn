package build.cairn.app.data

import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive

typealias SetupTask = build.cairn.contracts.SetupTask

typealias SetupPreset = build.cairn.contracts.SetupPreset

typealias SetupStep = build.cairn.contracts.SetupStep

typealias PluginSetup = build.cairn.contracts.PluginSetup

/** The setup resolved by the installation; invalid packages have no setup. */
fun PluginEntry.resolvedSetup(): PluginSetup = setup ?: PluginSetup()

/** The catalogue's words for what connecting takes. */
fun effortLabel(effort: String?): String? =
    when (effort) {
        "signin" -> "Se connecter"
        "token" -> "Coller un jeton"
        "app" -> "Votre propre app"
        else -> null
    }

fun riskLabel(risk: String?): String? =
    when (risk) {
        "low" -> "Risque faible"
        "medium" -> "Risque moyen"
        "high" -> "Risque élevé"
        else -> null
    }

/**
 * Host-written step titles in French; titles the package wrote stay as the package wrote them, like
 * its settings.
 */
fun SetupStep.shownTitle(manifest: PluginManifest): String =
    when (title) {
        "Settings" -> "Réglages"
        "Sign in with ${manifest.name}" -> "Se connecter avec ${manifest.name}"
        "Who can talk to it" -> "Qui peut lui parler"
        "Details" -> "Détails"
        else -> title
    }

/** The step's fields, in the step's order. */
fun SetupStep.fields(all: List<PluginSetting>): List<PluginSetting> = settings.mapNotNull { key ->
    all.find { it.key == key }
}

/** The preset the step's switches match, or "" when they were fine-tuned. */
fun SetupStep.matchingPreset(all: List<PluginSetting>, values: Map<String, String>): String {
    val switches = fields(all).filter { it.type == "boolean" }
    return presets
        .find { preset ->
            switches.all { field ->
                (values[field.key] == "true") == (preset.values?.get(field.key) == true)
            }
        }
        ?.id
        .orEmpty()
}

/** The step's switches as the preset sets them, as form text. */
fun SetupStep.presetValues(all: List<PluginSetting>, preset: SetupPreset): Map<String, String> =
    fields(all)
        .filter { it.type == "boolean" }
        .associate { it.key to (preset.values?.get(it.key) == true).toString() }

/** One app (or other shared setup) already saved, and the instances using it. */
data class SharedSource(
    /** The instance the installation copies the values from. */
    val id: String,
    val names: List<String>,
    /** The visible shared values, e.g. the OAuth client ID, to tell apps apart. */
    val label: String,
)

/**
 * Saved instances whose required shared values are complete. Secret values never reach clients, so
 * sources with secrets remain individually selectable even if their labels match.
 */
fun sharedSources(
    steps: List<SetupStep>,
    fields: List<PluginSetting>,
    connections: List<PluginConnection>,
    except: String? = null,
): List<SharedSource> {
    val shared = steps.filter { it.shared }.flatMap { it.fields(fields) }
    if (shared.isEmpty()) return emptyList()
    val sources = mutableListOf<SharedSource>()
    for (connection in connections) {
        if (connection.id == except) continue
        val complete = shared.all { field ->
            if (field.type == "secret") !field.required || field.key in connection.secretKeys
            else {
                val value = connection.settings[field.key]
                !field.required ||
                    (value != null && value != JsonNull && value != JsonPrimitive(""))
            }
        }
        if (!complete) continue
        val label =
            shared
                .filter { it.type != "secret" }
                .map { field ->
                    when (val value = connection.settings[field.key]) {
                        null,
                        JsonNull -> ""
                        is JsonPrimitive -> value.content
                        else -> value.toString()
                    }
                }
                .filter { it.isNotEmpty() }
                .joinToString(" · ")
        sources += SharedSource(connection.id, listOf(connection.name), label)
    }
    return sources
}

/** "4812…tent.com": long identifiers stay recognisable without filling a line. */
fun shortValue(value: String, keep: Int = 10): String =
    if (value.length > keep * 2 + 1) "${value.take(keep)}…${value.takeLast(keep)}" else value
