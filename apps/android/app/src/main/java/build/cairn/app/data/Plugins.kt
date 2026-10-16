package build.cairn.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull

@Serializable
data class PluginSetting(
    val key: String = "",
    val title: String = "",
    /** `string`, `number` or `boolean`. */
    val type: String = "string",
    val description: String = "",
    val required: Boolean = false,
    val hidden: Boolean = false,
    val maxLength: Int? = null,
    val minimum: Double? = null,
    val maximum: Double? = null,
)

@Serializable data class PluginCredential(val label: String = "", val header: String = "")

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
) {
    /** Instances answer external people through a dedicated agent. */
    val delegates
        get() = "conversations.submit" in capabilities

    val discord
        get() = "transport.discord" in capabilities
}

@Serializable
data class PluginVersion(
    val version: String = "",
    val digest: String? = null,
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

@Serializable data class PluginStatus(val state: String = "disconnected")

@Serializable data class PluginUsage(val hour: Int = 0)

/** One configured instance of a plugin, such as a Discord bot. */
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
    val status: PluginStatus = PluginStatus(),
    val usage: PluginUsage = PluginUsage(),
    val requests: List<PluginRequest> = emptyList(),
    val activity: List<PluginActivity> = emptyList(),
) {
    fun setting(key: String): String? =
        (settings[key] as? JsonPrimitive)?.contentOrNull?.takeIf { it.isNotBlank() }

    fun flag(key: String): Boolean = (settings[key] as? JsonPrimitive)?.booleanOrNull == true

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

@Serializable
data class OnePasswordAccount(
    val id: String = "",
    val name: String = "",
    val enabled: Boolean = true,
    val agentIds: List<String> = emptyList(),
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
        status.state == "connected" -> PluginBadge("En ligne", PluginTone.OK)
        status.state == "connecting" -> PluginBadge("Connexion…", PluginTone.MUTED)
        status.state == "needs_review" -> PluginBadge("À vérifier", PluginTone.ATTENTION)
        else -> PluginBadge("Hors ligne", PluginTone.ATTENTION)
    }

/** One plugin card in the catalogue: its status, a short count and pending requests. */
data class PluginSummary(val badge: PluginBadge, val meta: String, val requests: Int)

private fun plural(count: Int, noun: String) = "$count $noun${if (count > 1) "s" else ""}"

fun PluginEntry.summary(
    connections: List<PluginConnection>,
    onePassword: List<OnePasswordAccount>,
): PluginSummary {
    if (id == "onepassword") {
        val agents = onePassword.flatMap { it.agentIds }.distinct().size
        return if (onePassword.isEmpty())
            PluginSummary(
                PluginBadge("À configurer", PluginTone.MUTED),
                "Ajoutez un compte de service",
                0,
            )
        else
            PluginSummary(
                PluginBadge(plural(onePassword.size, "compte"), PluginTone.OK),
                "${plural(agents, "agent")} autorisé${if (agents > 1) "s" else ""}",
                0,
            )
    }
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
    val noun = if (manifest?.discord == true) "bot" else "instance"
    return PluginSummary(
        when {
            status == "update" -> PluginBadge("Mise à jour à examiner", PluginTone.WARNING)
            mine.isEmpty() -> PluginBadge("À configurer", PluginTone.MUTED)
            online > 0 -> PluginBadge("$online en ligne", PluginTone.OK)
            else -> mine.first().state()
        },
        if (mine.isEmpty()) "Version ${active.orEmpty()}" else plural(mine.size, noun),
        mine.sumOf { it.requests.size },
    )
}

/** How each host capability reads in the catalogue. */
fun capabilityLabel(name: String) =
    when (name) {
        "transport.discord" -> "Bot Discord"
        "conversations.submit" -> "Conversations"
        "conversations.control" -> "Répondre et arrêter"
        "http" -> "Requêtes web"
        "storage" -> "Stockage"
        "timers" -> "Minuteurs"
        "onepassword.read" -> "Secrets"
        else -> name
    }

/**
 * External people may only reach a dedicated agent: one project at most, no installation access.
 */
fun Agent.eligibleForPlugins() =
    !access.installation && access.projects != null && access.projects.size <= 1

/** Plugin settings as the server expects them: numbers as numbers, blanks omitted. */
fun pluginSettings(fields: List<PluginSetting>, values: Map<String, String>): JsonObject =
    JsonObject(
        fields
            .mapNotNull { field ->
                val value = values[field.key].orEmpty()
                val element: JsonElement? =
                    when (field.type) {
                        "boolean" -> JsonPrimitive(value == "true")
                        "number" -> value.trim().toDoubleOrNull()?.let(::number)
                        else -> value.takeIf { it.isNotEmpty() }?.let(::JsonPrimitive)
                    }
                element?.let { field.key to it }
            }
            .toMap()
    )

private fun number(value: Double) =
    if (value % 1.0 == 0.0 && kotlin.math.abs(value) < Long.MAX_VALUE) JsonPrimitive(value.toLong())
    else JsonPrimitive(value)

data class PluginAbility(val allowed: Boolean, val text: String, val warn: Boolean = false)

/** What a Discord bot lets people do, in the order the web lists it. */
fun PluginConnection.discordAbilities(
    agent: Agent?,
    project: String?,
    secrets: List<String>,
): List<PluginAbility> = buildList {
    if (accessMode == "everyone")
        add(
            PluginAbility(
                true,
                "Tout le monde dans le canal peut faire travailler l’agent (lecture seule)",
                warn = true,
            )
        )
    if (flag("everyMessage"))
        add(PluginAbility(true, "Répondre à chaque message, en silence quand rien n’est utile"))
    add(PluginAbility(true, "Démarrer des conversations avec ${agent?.name ?: "son agent"}"))
    add(
        if (project != null) PluginAbility(true, "Lire et modifier $project")
        else PluginAbility(false, "Ouvrir un projet")
    )
    add(
        PluginAbility(
            true,
            "Répondre dans #${setting("channelName") ?: setting("channelId").orEmpty()} et ses fils uniquement",
        )
    )
    add(
        PluginAbility(
            true,
            "Laisser les personnes autorisées répondre à ses questions et l’arrêter depuis Discord",
        )
    )
    add(
        if (secrets.isNotEmpty())
            PluginAbility(true, "Utiliser 1Password : ${secrets.joinToString(", ")}", warn = true)
        else PluginAbility(false, "Lire des secrets 1Password")
    )
    if (agent?.access?.github == true)
        add(PluginAbility(true, "Utiliser GitHub en votre nom", warn = true))
    add(PluginAbility(false, "Gérer l’installation"))
    add(PluginAbility(false, "Voir les autres conversations"))
}
