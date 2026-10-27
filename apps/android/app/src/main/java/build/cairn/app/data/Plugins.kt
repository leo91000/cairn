package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive

typealias PluginOption = build.cairn.contracts.PluginOption

/** One field of a declarative plugin form: instance settings or a view's form. */
@Serializable
data class PluginSetting(
    val key: String = "",
    val title: String = "",
    /** `string`, `text`, `number`, `boolean`, `secret`, `select`, `multiselect` or `url`. */
    val type: String = "string",
    val description: String = "",
    val placeholder: String = "",
    val section: String = "",
    val required: Boolean = false,
    val hidden: Boolean = false,
    val maxLength: Int? = null,
    val minimum: Double? = null,
    val maximum: Double? = null,
    val options: List<PluginOption> = emptyList(),
    /** Choices come from the package's form handler while the owner edits. */
    val dynamic: Boolean = false,
    /** Changing this field asks the package's form handler again. */
    val refresh: Boolean = false,
    /** How much a switch lets agents do: `low`, `medium` or `high`. */
    val risk: String? = null,
    val default: JsonElement? = null,
)

/** What a package's `form` handler returns while the owner edits an instance. */
typealias PluginForm = build.cairn.contracts.PluginFormResult

typealias PluginCredential = build.cairn.contracts.PluginCredential

typealias PluginProjectSource = build.cairn.contracts.PluginProjectSource

typealias PluginSkillSource = build.cairn.contracts.PluginSkillSource

typealias PluginBrand = build.cairn.contracts.PluginBrand

typealias PluginInstances = build.cairn.contracts.PluginInstances

typealias PluginProgram = build.cairn.contracts.PluginProgram

typealias ProgramRelease = build.cairn.contracts.ProgramRelease

typealias InstalledProgram = build.cairn.contracts.InstalledProgram

typealias ProgramInstallation = build.cairn.contracts.ProgramState

typealias ProgramInstallations = build.cairn.contracts.ListPluginProgramsOutput

typealias PluginAgentAccess = build.cairn.contracts.PluginAgentAccess

typealias PluginArgument = build.cairn.contracts.PluginArgument

typealias PluginCommand = build.cairn.contracts.PluginCommand

typealias PluginTool = build.cairn.contracts.PluginTool

typealias PluginManifest = build.cairn.contracts.PluginManifest

typealias PluginFile = build.cairn.contracts.PluginFile

typealias PluginVersion = build.cairn.contracts.PackageVersion

typealias PluginEntry = build.cairn.contracts.PluginEntry

val PluginManifest.delegates
    get() = "conversations.submit" in capabilities
val PluginManifest.needsWorker
    get() =
        listOf("socket", "inbound", "conversations.submit", "conversations.control", "timers").any {
            it in capabilities
        }
val PluginManifest.onePerAgent
    get() = runCredentials?.let { it.files.isNotEmpty() || it.git.isNotEmpty() } == true

fun PluginManifest.noun(count: Int = 1): String =
    instances?.let { if (count == 1) it.singular else it.plural }
        ?: if (count == 1) "Instance" else "Instances"

val PluginManifest.settingFields: List<PluginSetting>
    get() = settings.map { wireJson.decodeFromJsonElement(PluginSetting.serializer(), it!!) }
val PluginSkillSource.settingFields: List<PluginSetting>
    get() = fields.map { wireJson.decodeFromJsonElement(PluginSetting.serializer(), it!!) }
val PluginEntry.name
    get() = manifest?.name ?: id

fun PluginCommand.usage(plugin: String) =
    (listOf("cairn plugin", plugin, name) +
            arguments.map { argument ->
                val part = argument.flag?.let { "$it <${argument.name}>" } ?: "<${argument.name}>"
                if (argument.required) part else "[$part]"
            })
        .joinToString(" ")

typealias PluginLabel = build.cairn.contracts.PluginLabel

typealias PluginRequest = build.cairn.contracts.PluginRequest

typealias PluginActivity = build.cairn.contracts.PluginActivity

typealias PluginStatus = build.cairn.contracts.PluginStatus

typealias PluginUsage = build.cairn.contracts.PluginUsage

typealias PluginConnection = build.cairn.contracts.PluginConnection

fun PluginConnection.setting(key: String): String? =
    (settings[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }

fun PluginConnection.flag(key: String): Boolean =
    (settings[key] as? JsonPrimitive)?.booleanOrNull == true

/** A setting as people read it: lists joined, switches as Oui/Non. */
fun PluginConnection.display(key: String): String =
    when (val value = settings[key]) {
        null -> "—"
        is JsonArray ->
            value.mapNotNull { it.jsonPrimitive.contentOrNull }.joinToString(", ").ifBlank { "—" }
        is JsonPrimitive ->
            value.booleanOrNull?.let { if (it) "Oui" else "Non" }
                ?: value.contentOrNull?.ifBlank { null }
                ?: "—"
        else -> value.toString()
    }

fun PluginConnection.personName(id: String) =
    people.find { it.id == id }?.name?.takeIf { it.isNotBlank() } ?: "Utilisateur ${id.takeLast(4)}"

typealias PluginCapability = build.cairn.contracts.PluginCapability

typealias PluginCatalog = build.cairn.contracts.PluginCatalog

enum class PluginTone {
    OK,
    MUTED,
    ATTENTION,
    WARNING,
}

data class PluginBadge(val label: String, val tone: PluginTone)

/** What an instance is doing now, as the web shows it. */
fun PluginConnection.state(): PluginBadge =
    when {
        !enabled -> PluginBadge("En pause", PluginTone.MUTED)
        status.state == "ready" -> PluginBadge("Prêt", PluginTone.OK)
        status.state == "connected" -> PluginBadge("En ligne", PluginTone.OK)
        status.state == "connecting" -> PluginBadge("Connexion…", PluginTone.MUTED)
        status.state == "needs_review" -> PluginBadge("À vérifier", PluginTone.ATTENTION)
        else -> PluginBadge("Hors ligne", PluginTone.ATTENTION)
    }

/**
 * One plugin card in the catalogue: its status, a short count, pending requests and, until the
 * first instance exists, what connecting takes ([PluginSetup.effort]).
 */
data class PluginSummary(
    val badge: PluginBadge,
    val meta: String,
    val requests: Int,
    val effort: String? = null,
)

private fun plural(count: Int, noun: String) = "$count $noun${if (count > 1) "s" else ""}"

fun PluginEntry.summary(connections: List<PluginConnection>): PluginSummary {
    if (status == "needs_approval" || status == "invalid")
        return PluginSummary(
            if (status == "invalid") PluginBadge("Invalide", PluginTone.ATTENTION)
            else PluginBadge("À approuver", PluginTone.WARNING),
            if (manifest != null) "Version ${versions.lastOrNull()?.version.orEmpty()}"
            else "Vérifiez les fichiers du package",
            0,
        )
    val mine = connections.filter { it.pluginId == id }
    val online = mine.count { it.state().tone == PluginTone.OK }
    val noun = (manifest?.noun(mine.size) ?: "instances").lowercase()
    val agents = mine.flatMap { it.agentIds }.distinct().size
    val granted =
        if (manifest?.agentAccess != null)
            " · ${plural(agents, "agent")} autorisé${if (agents > 1) "s" else ""}"
        else ""
    return PluginSummary(
        when {
            status == "update" -> PluginBadge("Mise à jour à examiner", PluginTone.WARNING)
            mine.isEmpty() -> PluginBadge("À configurer", PluginTone.MUTED)
            online > 0 ->
                PluginBadge(
                    if (manifest?.needsWorker == false) "$online prêt${if (online > 1) "s" else ""}"
                    else "$online en ligne",
                    PluginTone.OK,
                )
            else -> mine.first().state()
        },
        if (mine.isEmpty()) "Version ${active.orEmpty()}" else "${mine.size} $noun$granted",
        mine.sumOf { it.requests.size },
        resolvedSetup().effort.takeIf { mine.isEmpty() },
    )
}

/** How each host capability reads in the catalogue. */
fun capabilityLabel(name: String) =
    when (name) {
        "socket" -> "Connexion permanente"
        "inbound" -> "Reçoit des messages"
        "conversations.submit" -> "Conversations"
        "conversations.control" -> "Répondre et arrêter"
        "http" -> "Requêtes web"
        "storage" -> "Stockage"
        "timers" -> "Minuteurs"
        "exec" -> "Exécute des programmes"
        else -> name
    }

/** What a package gives agents and the owner, for catalogue chips. */
fun PluginManifest.contributions(): List<String> = buildList {
    if (commands.isNotEmpty()) add("Commandes CLI")
    if (tools.isNotEmpty()) add("Outils pour agents")
    if (agentAccess != null) add("Accès par agent")
    if (instanceView) add("Tableau de bord")
}

/**
 * What the people reaching an agent through a bot could do with its access, for the owner's
 * warning. Nothing is refused: the owner chooses who may reach which agent. For anyone in a
 * channel, writing, MCP and plugin access matter as well.
 */
fun Agent.delegationRisks(
    connections: List<PluginConnection>,
    everyone: Boolean,
    draftGrants: List<String> = emptyList(),
): List<String> = buildList {
    if (access.installation) add("gérer toute votre installation")
    when {
        access.projects == null -> add("ouvrir tous les projets")
        access.projects.size > 1 -> add("ouvrir ${access.projects.size} projets")
    }
    if (!everyone) return@buildList
    if (access.sandbox != "read-only") add("modifier des fichiers et lancer des commandes")
    if (access.mcps == null || access.mcps.isNotEmpty()) add("utiliser ses connexions MCP")
    if (id in draftGrants || connections.any { id in it.agentIds })
        add("utiliser ses plugins, comme GitHub ou 1Password")
}

/** `a, b et c`. */
fun listed(items: List<String>): String =
    if (items.size > 1) items.dropLast(1).joinToString(", ") + " et " + items.last()
    else items.firstOrNull().orEmpty()

/**
 * Form values as the server expects them: numbers as numbers, blanks omitted. Multiple choices are
 * kept as a JSON array in their text value.
 */
fun pluginSettings(fields: List<PluginSetting>, values: Map<String, String>): JsonObject =
    JsonObject(
        fields
            .filter { it.type != "secret" }
            .mapNotNull { field ->
                val value = values[field.key].orEmpty()
                val element: JsonElement? =
                    when (field.type) {
                        "boolean" -> JsonPrimitive(value == "true")
                        "number" -> value.trim().toDoubleOrNull()?.let(::number)
                        "multiselect" -> multiselect(value)
                        "select" -> value.takeIf { it.isNotEmpty() }?.let(::JsonPrimitive)
                        else -> value.trim().takeIf { it.isNotEmpty() }?.let(::JsonPrimitive)
                    }
                element?.let { field.key to it }
            }
            .toMap()
    )

/** Whether a required field of a declarative form is still empty (switches always have a value). */
fun requiredMissing(fields: List<PluginSetting>, values: Map<String, String>): Boolean =
    fields.any {
        !it.hidden &&
            it.required &&
            it.type !in setOf("boolean", "secret", "multiselect") &&
            // A choice is an exact identifier; only text uses blankness.
            (if (it.type == "select") values[it.key].isNullOrEmpty()
            else values[it.key].isNullOrBlank())
    }

/** Blank secrets keep saved credentials; view actions preserve the typed value. */
fun pluginSecrets(values: Map<String, String>, trim: Boolean = true): JsonObject =
    JsonObject(
        values
            .filterValues { it.isNotBlank() }
            .mapValues { JsonPrimitive(if (trim) it.value.trim() else it.value) }
    )

/** The form text of a saved value or a field default. */
fun formValue(field: PluginSetting, saved: JsonElement?): String {
    val value = saved ?: field.default
    return when {
        value == null ->
            if (field.type == "boolean") "false" else if (field.type == "multiselect") "[]" else ""
        field.type == "multiselect" -> value.toString()
        value is JsonPrimitive -> value.contentOrNull.orEmpty()
        else -> ""
    }
}

fun multiselect(value: String): JsonArray = runCatching {
    wireJson.parseToJsonElement(value).jsonArray
}
    .getOrDefault(JsonArray(emptyList()))

private fun number(value: Double) =
    if (value % 1.0 == 0.0 && kotlin.math.abs(value) < Long.MAX_VALUE) JsonPrimitive(value.toLong())
    else JsonPrimitive(value)

data class PluginAbility(val allowed: Boolean, val text: String, val warn: Boolean = false)

/** What people reaching an instance can make its agent do, in the order the web lists it. */
fun PluginConnection.abilities(
    agent: Agent?,
    project: String?,
    plugins: List<String>,
): List<PluginAbility> = buildList {
    if (accessMode == "everyone")
        add(
            PluginAbility(
                true,
                "Tout le monde peut faire travailler l’agent (lecture seule)",
                warn = true,
            )
        )
    add(PluginAbility(true, "Démarrer des conversations avec ${agent?.name ?: "son agent"}"))
    add(
        if (project != null) PluginAbility(true, "Lire et modifier $project")
        else PluginAbility(false, "Ouvrir un projet")
    )
    add(
        if (plugins.isNotEmpty())
            PluginAbility(true, "Utiliser des plugins : ${plugins.joinToString(", ")}", warn = true)
        else PluginAbility(false, "Utiliser des plugins comme 1Password")
    )
    add(PluginAbility(false, "Gérer l’installation"))
    add(PluginAbility(false, "Voir les autres conversations"))
}

/** Plugins (such as 1Password) an agent is authorized on, as "Plugin · Instance". */
fun PluginCatalog.grantsOf(agentId: String): List<String> =
    connections
        .filter { agentId in it.agentIds }
        .map { connection ->
            val plugin = packages.find { it.id == connection.pluginId }?.name ?: connection.pluginId
            "$plugin · ${connection.name}"
        }

/**
 * A plugin's device sign-in as the installation shows it (`POST /plugins/{id}/device-flow`): the
 * token never reaches the app. [state] is `pending`, `completed`, `failed` or `expired`.
 */
typealias PluginDeviceFlow = build.cairn.contracts.PluginSigninFlow

/** The page where the owner enters the code; some services prefill it. */
val PluginDeviceFlow.link
    get() = verificationUriComplete?.takeIf { it.isNotBlank() } ?: verificationUri

val PluginDeviceFlow.pending
    get() = state == "pending" || state == "exchanging"

val PluginDeviceFlow.completed
    get() = state == "completed"

/**
 * What a form call or a save sends for the credential: a completed sign-in replaces a pasted token,
 * and neither is sent when the saved credential stays.
 */
fun credentialInput(token: String, flow: PluginDeviceFlow?): Pair<String, String>? =
    when {
        flow?.completed == true -> "credentialFlow" to flow.id
        token.isNotBlank() -> "token" to token.trim()
        else -> null
    }

/** Why a sign-in ended without a credential, or null while it can still complete. */
fun signInProblem(flow: PluginDeviceFlow?): String? =
    when (flow?.state) {
        "expired" -> flow.message?.ifBlank { null } ?: "Cette connexion a expiré. Recommencez."
        "failed" -> flow.message?.ifBlank { null } ?: "La connexion a échoué. Réessayez."
        else -> null
    }

/** A project source of one package, with the enabled instances that can import from it. */
data class ProjectSourceChoice(
    val entry: PluginEntry,
    val source: PluginProjectSource,
    val instances: List<PluginConnection>,
) {
    val key
        get() = "${entry.id}/${source.name}"

    /** "GitHub · Repositories", or the source alone when it names the package already. */
    val label
        get() = sourceLabel(entry.name, source.title)
}

/** A skill source of one package, with the enabled instances that can import from it. */
data class SkillSourceChoice(
    val entry: PluginEntry,
    val source: PluginSkillSource,
    val instances: List<PluginConnection>,
) {
    val key
        get() = "${entry.id}/${source.name}"

    val label
        get() = sourceLabel(entry.name, source.title)
}

private fun sourceLabel(plugin: String, title: String) =
    when {
        title.isBlank() -> plugin
        title.contains(plugin, ignoreCase = true) -> title
        else -> "$plugin · $title"
    }

private fun PluginEntry.runs() =
    active != null && manifest != null && (status == "active" || status == "update")

/** Project sources the owner can import from: approved packages with an enabled instance. */
fun PluginCatalog.projectSources(): List<ProjectSourceChoice> =
    packages
        .filter { it.runs() }
        .flatMap { entry ->
            val instances = connections.filter { it.pluginId == entry.id && it.enabled }
            entry.manifest
                ?.projectSources
                .orEmpty()
                .filter { it.name.isNotBlank() }
                .map { ProjectSourceChoice(entry, it, instances) }
        }
        .filter { it.instances.isNotEmpty() }

/** Skill sources the owner can import from: approved packages with an enabled instance. */
fun PluginCatalog.skillSources(): List<SkillSourceChoice> =
    packages
        .filter { it.runs() }
        .flatMap { entry ->
            val instances = connections.filter { it.pluginId == entry.id && it.enabled }
            entry.manifest
                ?.skillSources
                .orEmpty()
                .filter { it.name.isNotBlank() }
                .map { SkillSourceChoice(entry, it, instances) }
        }
        .filter { it.instances.isNotEmpty() }

/** One project a plugin's project source offers for import. */
@Serializable
data class PluginProject(
    val id: String = "",
    val name: String = "",
    val description: String = "",
    val defaultBranch: String = "",
    /** The plugin's short labels, e.g. "Private", "Fork", "Rust". */
    val badges: List<String> = emptyList(),
    val updatedAt: String? = null,
    val url: String? = null,
    /** Already registered: shown, not selectable. */
    val imported: Boolean = false,
    /** The name a project imported from this item takes. */
    val projectName: String? = null,
)

@Serializable
data class PluginProjectPage(
    val items: List<PluginProject> = emptyList(),
    val nextPage: Int? = null,
)

/**
 * A loaded project matches when every word typed appears in its name, identifier, description or
 * badges, ignoring case and accents.
 */
fun PluginProject.matches(query: String): Boolean {
    val words = searchable(query).split(Regex("\\s+")).filter { it.isNotBlank() }
    val text = searchable(listOf(name, id, description, *badges.toTypedArray()).joinToString(" "))
    return words.all { it in text }
}

private fun searchable(value: String) =
    java.text.Normalizer.normalize(value, java.text.Normalizer.Form.NFD)
        .replace(Regex("\\p{M}+"), "")
        .lowercase()

/** `GET /plugins/{id}/projects` for one page of a source, through one instance. */
fun pluginProjectsPath(pluginId: String, source: String, instance: String, page: Int): String =
    "/plugins/${segment(pluginId)}/projects?source=${segment(source)}&instance=${segment(instance)}&page=$page"
