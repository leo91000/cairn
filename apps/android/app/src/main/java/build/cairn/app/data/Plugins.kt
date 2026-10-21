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

@Serializable
data class PluginOption(
    val value: String = "",
    val label: String = "",
    /** Shown under the choice, e.g. why it can't be picked. */
    val description: String = "",
    val disabled: Boolean = false,
)

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
    val default: JsonElement? = null,
)

/** What a package's `form` handler returns while the owner edits an instance. */
@Serializable
data class PluginForm(
    val options: Map<String, List<PluginOption>> = emptyMap(),
    val values: JsonObject = JsonObject(emptyMap()),
    val errors: Map<String, String> = emptyMap(),
    val view: JsonArray = JsonArray(emptyList()),
)

@Serializable
data class PluginCredential(
    val label: String = "",
    val header: String = "",
    val prefix: String = "",
    /**
     * The owner can sign in on the service's page instead of pasting a token. Only its presence
     * matters to the app, so its shape is not checked here.
     */
    val deviceFlow: JsonObject? = null,
)

/** A named source a package offers, such as a service's repositories. */
@Serializable
data class PluginProjectSource(
    val name: String = "",
    val title: String = "",
    val description: String = "",
)

/** Where a package's instances import skills from, with the location's fields. */
@Serializable
data class PluginSkillSource(
    val name: String = "",
    val title: String = "",
    val description: String = "",
    val fields: List<PluginSetting> = emptyList(),
)

/** The package's mark: a background colour and an SVG path on a 24×24 grid. */
@Serializable data class PluginBrand(val color: String = "", val path: String = "")

@Serializable data class PluginInstances(val singular: String = "", val plural: String = "")

@Serializable
data class PluginProgram(
    val name: String = "",
    val path: String = "",
    val description: String = "",
    val installer: String = "",
)

@Serializable
data class ProgramRelease(
    val version: String = "",
    val url: String = "",
    val sha256: String? = null,
)

@Serializable
data class InstalledProgram(
    val release: ProgramRelease = ProgramRelease(),
    val archiveSha256: String = "",
    val binarySha256: String = "",
)

@Serializable
data class ProgramInstallation(
    val name: String = "",
    val installer: String = "",
    val installed: InstalledProgram? = null,
    val previous: InstalledProgram? = null,
    val candidate: ProgramRelease? = null,
    val digest: String? = null,
)

@Serializable data class ProgramInstallations(val programs: List<ProgramInstallation> = emptyList())

@Serializable data class PluginAgentAccess(val description: String = "")

@Serializable
data class PluginArgument(
    val name: String = "",
    val flag: String? = null,
    val positional: Boolean = false,
    val required: Boolean = false,
    val description: String = "",
)

/** A command agents run in their conversation: `cairn plugin <plugin> <name> …`. */
@Serializable
data class PluginCommand(
    val name: String = "",
    val description: String = "",
    /** `json`, `text` or `secret`. */
    val output: String = "json",
    val arguments: List<PluginArgument> = emptyList(),
) {
    fun usage(plugin: String) =
        (listOf("cairn plugin", plugin, name) +
                arguments.map { argument ->
                    val part =
                        argument.flag?.let { "$it <${argument.name}>" } ?: "<${argument.name}>"
                    if (argument.required) part else "[$part]"
                })
            .joinToString(" ")
}

@Serializable data class PluginTool(val name: String = "", val description: String = "")

@Serializable
data class PluginManifest(
    val id: String = "",
    val name: String = "",
    val version: String = "",
    val description: String = "",
    val capabilities: List<String> = emptyList(),
    val hosts: List<String> = emptyList(),
    val credential: PluginCredential? = null,
    val settings: List<PluginSetting> = emptyList(),
    val instances: PluginInstances? = null,
    val brand: PluginBrand? = null,
    val programs: List<PluginProgram> = emptyList(),
    val agentAccess: PluginAgentAccess? = null,
    val commands: List<PluginCommand> = emptyList(),
    val tools: List<PluginTool> = emptyList(),
    val instructions: String = "",
    val instanceView: Boolean = false,
    val instanceTest: Boolean = false,
    val instanceForm: Boolean = false,
    /** Where its enabled instances import projects from. */
    val projectSources: List<PluginProjectSource> = emptyList(),
    /** Where its enabled instances import skills from. */
    val skillSources: List<PluginSkillSource> = emptyList(),
) {
    /** Instances answer external people through a dedicated agent. */
    val delegates
        get() = "conversations.submit" in capabilities

    /** Packages that react to people, connections or timers keep a worker running. */
    val needsWorker
        get() =
            listOf("socket", "inbound", "conversations.submit", "conversations.control", "timers")
                .any { it in capabilities }

    /** The package's own name for one or several instances, e.g. "Service account". */
    fun noun(count: Int = 1): String =
        instances?.let { if (count == 1) it.singular else it.plural }
            ?: if (count == 1) "Instance" else "Instances"
}

/** One Luau file of a package, for review. */
@Serializable data class PluginFile(val path: String = "", val bytes: Int = 0)

@Serializable
data class PluginVersion(
    val version: String = "",
    val digest: String? = null,
    /** The package's Luau files, `main.luau` first. */
    val files: List<PluginFile>? = null,
    val manifest: PluginManifest? = null,
    val approved: Boolean? = null,
    val error: String? = null,
)

/** A bundled or installed package and the owner's approval state. */
@Serializable
data class PluginEntry(
    val id: String = "",
    val builtin: Boolean = false,
    /** `active`, `update`, `needs_approval` or `invalid`. */
    val status: String = "active",
    val active: String? = null,
    val manifest: PluginManifest? = null,
    val versions: List<PluginVersion> = emptyList(),
) {
    val name
        get() = manifest?.name ?: id
}

@Serializable data class PluginLabel(val id: String = "", val name: String = "")

@Serializable
data class PluginRequest(
    val id: String = "",
    val userId: String = "",
    val name: String = "",
    val mentions: Int = 0,
    val updatedAt: Long = 0,
)

@Serializable
data class PluginActivity(
    val id: String = "",
    val chatId: String = "",
    val userId: String = "",
    val name: String = "",
    val preview: String = "",
    /** `working`, `replied`, `failed` or `stopped`. */
    val status: String = "",
    val createdAt: Long = 0,
)

/** Live state, with the package's own message (e.g. who it is signed in as). */
@Serializable
data class PluginStatus(val state: String = "disconnected", val message: String? = null)

@Serializable data class PluginUsage(val hour: Int = 0)

/** One configured instance of a plugin, such as a Discord bot or a service account. */
@Serializable
data class PluginConnection(
    val id: String = "",
    val pluginId: String = "",
    val name: String = "",
    val enabled: Boolean = true,
    val settings: JsonObject = JsonObject(emptyMap()),
    val userIds: List<String> = emptyList(),
    val people: List<PluginLabel> = emptyList(),
    /** `allowlist`, `requests` or `everyone`. */
    val accessMode: String = "allowlist",
    val maxPerPersonPerHour: Int? = null,
    val agentId: String = "",
    val projectId: String? = null,
    val maxMessagesPerHour: Int = 20,
    /** Agents granted the package's commands and tools on this instance. */
    val agentIds: List<String> = emptyList(),
    /** Secret fields saved in the vault; values never reach the app. */
    val secretKeys: List<String> = emptyList(),
    val status: PluginStatus = PluginStatus(),
    val usage: PluginUsage = PluginUsage(),
    val requests: List<PluginRequest> = emptyList(),
    val activity: List<PluginActivity> = emptyList(),
) {
    fun setting(key: String): String? =
        (settings[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }

    fun flag(key: String): Boolean = (settings[key] as? JsonPrimitive)?.booleanOrNull == true

    /** A setting as people read it: lists joined, switches as Oui/Non. */
    fun display(key: String): String =
        when (val value = settings[key]) {
            null -> "—"
            is JsonArray ->
                value
                    .mapNotNull { it.jsonPrimitive.contentOrNull }
                    .joinToString(", ")
                    .ifBlank { "—" }
            is JsonPrimitive ->
                value.booleanOrNull?.let { if (it) "Oui" else "Non" }
                    ?: value.contentOrNull?.ifBlank { null }
                    ?: "—"
            else -> value.toString()
        }

    fun personName(id: String) =
        people.find { it.id == id }?.name?.takeIf { it.isNotBlank() }
            ?: "Utilisateur ${id.takeLast(4)}"
}

@Serializable data class PluginCapability(val name: String = "", val description: String = "")

@Serializable
data class PluginCatalog(
    val packages: List<PluginEntry> = emptyList(),
    val capabilities: List<PluginCapability> = emptyList(),
    val connections: List<PluginConnection> = emptyList(),
)

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

/** One plugin card in the catalogue: its status, a short count and pending requests. */
data class PluginSummary(val badge: PluginBadge, val meta: String, val requests: Int)

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
 * External people may only reach a dedicated agent: one project at most, no installation access.
 */
fun Agent.eligibleForPlugins() =
    !access.installation && access.projects != null && access.projects.size <= 1

/**
 * Why an agent can't answer anyone who reaches an instance, or null when it can. Mirrors the
 * server: read-only sandbox, one project at most, no installation, MCP or plugin grants.
 */
fun Agent.publicProblem(connections: List<PluginConnection>): String? =
    when {
        !eligibleForPlugins() -> "Trop large"
        access.sandbox != "read-only" -> "Pas en lecture seule"
        access.mcps == null || access.mcps.isNotEmpty() -> "A des MCP"
        connections.any { id in it.agentIds } -> "A des plugins"
        else -> null
    }

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
 * The grants an edited instance must send back unchanged, or null to omit them. An older
 * installation reads a missing list as "no agents"; a current one keeps the grants when omitted.
 * With the agents not loaded yet (Main always exists) the installation is unknown, so they are kept
 * as for an older one. Only packages giving agents access have grants to keep.
 */
fun PluginConnection.agentIdsToKeep(agents: List<Agent>, manifest: PluginManifest): List<String>? =
    agentIds.takeIf {
        it.isNotEmpty() &&
            manifest.agentAccess != null &&
            (agents.isEmpty() || olderInstallation(agents))
    }

/**
 * The grants to send back on an older or unknown installation, always read again just before saving
 * ([current], null when the instance no longer exists) whatever the editor last saw: a grant
 * revoked meanwhile is never restored, one added meanwhile is never dropped.
 */
suspend fun PluginConnection.agentIdsToSend(
    agents: List<Agent>,
    manifest: PluginManifest,
    current: suspend () -> List<String>?,
): List<String>? {
    val keeps = manifest.agentAccess != null && (agents.isEmpty() || olderInstallation(agents))
    if (!keeps) return null
    val fresh = current() ?: return null
    return copy(agentIds = fresh).agentIdsToKeep(agents, manifest)
}

/**
 * A plugin's device sign-in as the installation shows it (`POST /plugins/{id}/device-flow`): the
 * token never reaches the app. [state] is `pending`, `completed`, `failed` or `expired`.
 */
@Serializable
data class PluginDeviceFlow(
    val id: String = "",
    val pluginId: String = "",
    val state: String = "pending",
    val userCode: String = "",
    val verificationUri: String = "",
    val verificationUriComplete: String? = null,
    val expiresAt: Long = 0,
    /** Why it failed, or the service's guidance. */
    val message: String? = null,
) {
    /** The page where the owner enters the code; some services prefill it. */
    val link
        get() = verificationUriComplete?.takeIf { it.isNotBlank() } ?: verificationUri

    val pending
        get() = state == "pending"

    val completed
        get() = state == "completed"
}

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
