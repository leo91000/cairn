package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive

/** Something the owner does on the service's site, ticked off in the assistant. */
@Serializable
data class SetupTask(
    val title: String = "",
    val text: String = "",
    /** The service's page for this task, HTTPS. */
    val url: String? = null,
    /** The link's label, e.g. "Open API Library". */
    val link: String? = null,
    /** A value Cairn shows for the owner to copy into the service: `callbackUrl`. */
    val copy: String? = null,
)

/** A named combination of a step's switches, e.g. "Read only". */
@Serializable
data class SetupPreset(
    val id: String = "",
    val title: String = "",
    val description: String = "",
    /** `low`, `medium` or `high`. */
    val risk: String? = null,
    /** Switches turned on; the step's other switches are turned off. */
    val values: Map<String, Boolean> = emptyMap(),
)

/** One step of the setup assistant. */
@Serializable
data class SetupStep(
    val id: String = "",
    /** `settings`, `credential`, `people` or `agents`. */
    val kind: String = "settings",
    val title: String = "",
    val description: String = "",
    /** Keys of the settings this step shows, in order. */
    val settings: List<String> = emptyList(),
    /** The same values serve every instance; a new one can reuse them from another. */
    val shared: Boolean = false,
    val checklist: List<SetupTask> = emptyList(),
    val presets: List<SetupPreset> = emptyList(),
    /** A consequence to know before going on, e.g. a provider's token expiry. */
    val warning: String = "",
)

/**
 * What connecting an instance takes and the steps the owner follows, as the installation resolves
 * them (`setup` next to each package's manifest).
 */
@Serializable
data class PluginSetup(
    /** `signin`, `token` or `app`: a sign-in, a pasted token, or an app registered first. */
    val effort: String? = null,
    val summary: String = "",
    /** A string setting naming a new instance once known, e.g. the verified mailbox. */
    val nameFrom: String? = null,
    val steps: List<SetupStep> = emptyList(),
)

/**
 * The setup the owner follows. An older installation sends only the manifest: resolve it the same
 * way, the package's declared steps first, else the default ones.
 */
fun PluginEntry.resolvedSetup(): PluginSetup = resolvedSetup(manifest, setup)

internal fun PluginConfigurationEntry.resolvedSetup(): PluginSetup = resolvedSetup(manifest, setup)

private fun resolvedSetup(manifest: PluginManifest?, setup: PluginSetup?): PluginSetup {
    setup?.let {
        return it
    }
    if (manifest == null) return PluginSetup()
    val derived = manifest.defaultSetup()
    val declared = manifest.setup ?: return derived
    return declared.copy(
        effort = declared.effort ?: derived.effort,
        steps = declared.steps.ifEmpty { derived.steps },
    )
}

/** The steps the installation derives for packages that declare none. */
fun PluginManifest.defaultSetup(): PluginSetup {
    val visible = settings.filter { !it.hidden }.map { it.key }
    val fields =
        if (visible.isEmpty()) emptyList()
        else listOf(SetupStep("settings", "settings", "Settings", settings = visible))
    val signIn =
        credential?.let {
            val title =
                if (it.oauth != null || it.deviceFlow != null) "Sign in with $name" else it.label
            listOf(SetupStep("credential", "credential", title))
        } ?: emptyList()
    // An OAuth sign-in needs its client settings first; other forms read the credential to offer
    // their choices.
    val steps =
        (if (credential?.oauth != null) fields + signIn else signIn + fields).toMutableList()
    if (delegates) steps += SetupStep("people", "people", "Who can talk to it")
    if (agentAccess != null) steps += SetupStep("agents", "agents", "Agents")
    val secret = settings.any { it.type == "secret" && it.required }
    val effort =
        when {
            credential?.oauth != null -> "app"
            credential?.deviceFlow != null -> "signin"
            credential != null || secret -> "token"
            else -> null
        }
    return PluginSetup(effort = effort, steps = steps)
}

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
                (values[field.key] == "true") == (preset.values[field.key] == true)
            }
        }
        ?.id
        .orEmpty()
}

/** The step's switches as the preset sets them, as form text. */
fun SetupStep.presetValues(all: List<PluginSetting>, preset: SetupPreset): Map<String, String> =
    fields(all)
        .filter { it.type == "boolean" }
        .associate { it.key to (preset.values[it.key] == true).toString() }

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
