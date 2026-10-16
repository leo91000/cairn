@file:OptIn(
    ExperimentalLayoutApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import build.cairn.app.data.*
import kotlinx.serialization.json.*

/** The installation's web page for steps that stay on the web: package uploads, bot setup. */
internal fun webPage(api: CairnApi, path: String): String =
    api.origin
        .newBuilder()
        .encodedPath(api.installationId?.let { "/installations/${segment(it)}$path" } ?: path)
        .build()
        .toString()

/** Plugins: what gives the agents new abilities and new places to work. Owner only. */
@Composable
fun PluginsScreen(vm: CairnViewModel, state: Workspace, open: (String) -> Unit) {
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    var onePassword by remember { mutableStateOf<List<OnePasswordAccount>>(emptyList()) }
    val context = LocalContext.current
    Poll("plugins", 10_000) {
        try {
            catalog = vm.api.get("/plugins")
            onePassword = vm.api.get("/onepassword")
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Column(
        Modifier.fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag("plugins")
            .padding(horizontal = 16.dp)
            .padding(top = 4.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        ScreenTitle(
            "Plugins",
            "De nouvelles capacités et de nouveaux lieux de travail pour vos agents.",
            Modifier.padding(start = 4.dp, bottom = 10.dp),
        )
        Eyebrow("Installés", Modifier.padding(start = 4.dp))
        val current = catalog
        if (current == null) LinearProgressIndicator(Modifier.fillMaxWidth())
        else
            current.packages.forEach { entry ->
                PluginCard(entry, entry.summary(current.connections, onePassword)) {
                    vm.clearMessage()
                    open(entry.id)
                }
            }
        SignalCard(Modifier.fillMaxWidth().padding(top = 8.dp)) {
            Text("Installer un package", style = MaterialTheme.typography.titleSmall)
            Text(
                "Un plugin est un manifest.json et un main.luau. L’envoi des fichiers se fait sur le web, puis vous l’approuvez ici ou sur le web.",
                Modifier.padding(top = 4.dp, bottom = 12.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            SignalButton(
                "Continuer sur le web",
                icon = CairnIcons.External,
                container = MaterialTheme.colorScheme.surface,
                content = MaterialTheme.colorScheme.onSurface,
                border = true,
                height = 40.dp,
            ) {
                runCatching { browse(context, webPage(vm.api, "/plugins")) }
            }
        }
    }
}

@Composable
private fun PluginCard(entry: PluginEntry, summary: PluginSummary, open: () -> Unit) {
    SignalCard(Modifier.fillMaxWidth().testTag("plugin-${entry.id}"), onClick = open) {
        Row(verticalAlignment = Alignment.Top) {
            PluginMark(entry.id, 44.dp)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        entry.name,
                        Modifier.weight(1f, fill = false),
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.width(8.dp))
                    Pill(
                        if (entry.builtin) "Intégré" else "Installé",
                        MaterialTheme.colorScheme.surfaceVariant,
                        MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Text(
                    entry.manifest?.description?.ifBlank { null } ?: "Aucune description.",
                    Modifier.padding(top = 2.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Spacer(Modifier.height(12.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            ToneChip(summary.badge)
            if (summary.requests > 0) {
                Spacer(Modifier.width(6.dp))
                Pill(
                    "${summary.requests} demande${if (summary.requests > 1) "s" else ""}",
                    signal.attentionSoft,
                    signal.attention,
                )
            }
            Spacer(Modifier.width(8.dp))
            Text(
                summary.meta,
                Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Icon(
                CairnIcons.Right,
                null,
                Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** One plugin: 1Password has its own screen, every other plugin lists its instances. */
@Composable
fun PluginScreen(vm: CairnViewModel, state: Workspace, id: String, openChat: (String) -> Unit) {
    if (id == "onepassword") OnePasswordScreen(vm, state)
    else PluginInstances(vm, state, id, openChat)
}

@Composable
private fun OnePasswordScreen(vm: CairnViewModel, state: Workspace) {
    var manifest by remember { mutableStateOf<PluginManifest?>(null) }
    LaunchedEffect(Unit) {
        manifest =
            runCatching { vm.api.get<PluginCatalog>("/plugins") }
                .getOrNull()
                ?.packages
                ?.find { it.id == "onepassword" }
                ?.manifest
    }
    Column(
        Modifier.fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag("plugin-onepassword-screen")
            .padding(horizontal = 16.dp)
            .padding(top = 4.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        PluginHeader(
            "onepassword",
            "1Password",
            builtin = true,
            version = manifest?.version,
            description =
                manifest?.description
                    ?: "Les agents envoient les secrets directement dans leurs commandes. Les valeurs n’apparaissent jamais dans la conversation.",
            capabilities = listOf("Outil CLI", "Secrets", "Accès par agent"),
        )
        SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(0.dp)) {
            OnePasswordAccounts(vm, state)
        }
        SignalCard(Modifier.fillMaxWidth()) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    CairnIcons.Terminal,
                    null,
                    Modifier.size(18.dp),
                    tint = MaterialTheme.colorScheme.primary,
                )
                Spacer(Modifier.width(8.dp))
                Text("Comment les agents l’utilisent", style = MaterialTheme.typography.titleSmall)
            }
            Text(
                "Un agent autorisé lit un secret avec la CLI Cairn et l’envoie dans la commande qui en a besoin :",
                Modifier.padding(top = 6.dp, bottom = 10.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Surface(
                shape = RoundedCornerShape(12.dp),
                color = MaterialTheme.colorScheme.surfaceVariant,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Box(Modifier.padding(12.dp)) {
                    Code(
                        "cairn plugin onepassword accounts\n" +
                            "cairn plugin onepassword read \\\n" +
                            "  --account ACCOUNT_ID \\\n" +
                            "  op://vault/item/field \\\n" +
                            "  | docker login --password-stdin …"
                    )
                }
            }
            Spacer(Modifier.height(10.dp))
            Ability(
                PluginAbility(
                    true,
                    "Les valeurs vont dans la commande, jamais dans la conversation.",
                )
            )
            Ability(
                PluginAbility(
                    true,
                    "Les tokens restent chiffrés sur l’installation. Les agents n’ont qu’un accès en lecture.",
                )
            )
        }
    }
}

@Composable
private fun PluginInstances(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openChat: (String) -> Unit,
) {
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    var onePassword by remember { mutableStateOf<List<OnePasswordAccount>>(emptyList()) }
    var editing by remember { mutableStateOf<PluginConnection?>(null) }
    var removing by remember { mutableStateOf<PluginConnection?>(null) }
    var reviewing by remember { mutableStateOf(false) }
    val context = LocalContext.current
    suspend fun load() {
        val next = vm.api.get<PluginCatalog>("/plugins")
        if (next.packages.find { it.id == id }?.manifest?.discord == true)
            onePassword = vm.api.get("/onepassword")
        catalog = next
    }
    Poll("plugin-$id", 5000) {
        if (editing == null && !reviewing && !state.busy)
            try {
                load()
            } catch (e: Exception) {
                vm.report(e)
            }
    }
    val entry = catalog?.packages?.find { it.id == id }
    val manifest = entry?.manifest
    val instances = catalog?.connections.orEmpty().filter { it.pluginId == id }
    Column(
        Modifier.fillMaxSize()
            .verticalScroll(rememberScrollState())
            .testTag("plugin-screen")
            .padding(horizontal = 16.dp)
            .padding(top = 4.dp, bottom = 32.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (catalog == null) {
            LinearProgressIndicator(Modifier.fillMaxWidth())
            return@Column
        }
        if (entry == null || manifest == null) {
            Empty(
                "Plugin introuvable",
                "Ce plugin n’est pas installé ou ses fichiers sont invalides.",
            )
            if (entry != null)
                SignalButton("Examiner le package", height = 40.dp) { reviewing = true }
            return@Column
        }
        PluginHeader(
            entry.id,
            manifest.name,
            entry.builtin,
            entry.active ?: manifest.version,
            manifest.description,
            manifest.capabilities.map(::capabilityLabel),
        )
        if (entry.status != "active")
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = signal.warningSoft,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Row(
                    Modifier.padding(start = 14.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        if (entry.status == "update")
                            "Une nouvelle version attend votre validation."
                        else "Validez ce plugin avant d’ajouter une instance.",
                        Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        color = signal.warning,
                    )
                    TextButton(onClick = { reviewing = true }) { Text("Examiner") }
                }
            }
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            SignalButton(
                "Accès et versions",
                icon = CairnIcons.Shield,
                container = MaterialTheme.colorScheme.surface,
                content = MaterialTheme.colorScheme.onSurface,
                border = true,
                height = 40.dp,
            ) {
                vm.clearMessage()
                reviewing = true
            }
            if (entry.active != null)
                if (manifest.discord)
                    SignalButton("Ajouter un bot", icon = CairnIcons.External, height = 40.dp) {
                        runCatching { browse(context, webPage(vm.api, "/plugins/discord/new")) }
                    }
                else
                    SignalButton("Ajouter une instance", icon = CairnIcons.Plus, height = 40.dp) {
                        vm.clearMessage()
                        editing = PluginConnection(name = manifest.name)
                    }
        }
        if (instances.isEmpty() && entry.active != null)
            Empty(
                if (manifest.discord) "Aucun bot" else "Aucune instance",
                if (manifest.discord)
                    "La création d’un bot (jeton, serveur, canal, accès) se fait sur le web. Vous le gérez ensuite ici."
                else "Ajoutez une instance pour commencer à utiliser ce plugin.",
            )
        instances.forEach { connection ->
            InstanceCard(
                vm,
                state,
                manifest,
                connection,
                onePassword,
                openChat = openChat,
                edit = {
                    vm.clearMessage()
                    if (manifest.discord)
                        runCatching {
                            browse(
                                context,
                                webPage(vm.api, "/plugins/discord/${segment(connection.id)}/edit"),
                            )
                        }
                    else editing = connection
                },
                remove = {
                    vm.clearMessage()
                    removing = connection
                },
                changed = { load() },
            )
        }
    }
    if (reviewing && entry != null)
        PluginReview(
            vm,
            state,
            entry,
            catalog?.capabilities.orEmpty(),
            close = { reviewing = false },
        ) {
            load()
        }
    editing?.let { initial ->
        manifest?.let {
            InstanceEditor(vm, state, id, it, initial, close = { editing = null }) {
                editing = null
                load()
            }
        }
    }
    removing?.let { connection ->
        val bot = manifest?.discord == true
        Confirm(
            if (bot) "Retirer ${connection.name} ?" else "Supprimer ${connection.name} ?",
            if (bot)
                "Le bot cesse de recevoir du travail et de livrer ses réponses, et son jeton est supprimé. Les conversations restent dans Cairn."
            else
                "L’instance s’arrête ; son identifiant, ses valeurs enregistrées et ses minuteurs sont supprimés. Les conversations restent dans Cairn.",
            state.busy,
            state.error,
            dismiss = { removing = null },
        ) {
            vm.perform {
                api.request(
                    "DELETE",
                    "/plugins/${segment(id)}/connections/${segment(connection.id)}",
                )
                removing = null
                load()
            }
        }
    }
}

@Composable
private fun InstanceCard(
    vm: CairnViewModel,
    state: Workspace,
    manifest: PluginManifest,
    connection: PluginConnection,
    onePassword: List<OnePasswordAccount>,
    openChat: (String) -> Unit,
    edit: () -> Unit,
    remove: () -> Unit,
    changed: suspend () -> Unit,
) {
    val path = "/plugins/${segment(connection.pluginId)}/connections/${segment(connection.id)}"
    val agent = state.agents.find { it.id == connection.agentId }
    val project = state.projects.find { it.id == connection.projectId }
    val badge = connection.state()
    fun person(userId: String, action: String) = vm.perform {
        api.request("POST", "$path/people/${segment(userId)}/$action")
        changed()
    }
    SignalCard(
        Modifier.fillMaxWidth().semantics { contentDescription = connection.name },
        padding = PaddingValues(18.dp),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(
                        connection.name,
                        style = MaterialTheme.typography.titleMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (manifest.discord)
                        Text(
                            connection.setting("botName")?.let { "Connecté en tant que $it" }
                                ?: "Bot Discord",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
                ToneChip(badge)
            }
            if (connection.enabled && connection.status.state == "needs_review")
                Text(
                    if (manifest.discord)
                        "Discord ou Cairn a arrêté ce bot. Le jeton a peut-être été réinitialisé ou l’accès de l’agent a changé. Modifiez la configuration sur le web pour la vérifier."
                    else "Cairn a arrêté cette instance. Modifiez-la pour la vérifier.",
                    Modifier.clip(RoundedCornerShape(12.dp))
                        .background(signal.attentionSoft)
                        .padding(12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = signal.attention,
                )
            if (connection.requests.isNotEmpty()) {
                Eyebrow("En attente de validation")
                connection.requests.forEach { request ->
                    val name = request.name.ifBlank { request.userId }
                    Row(
                        Modifier.fillMaxWidth()
                            .clip(RoundedCornerShape(14.dp))
                            .background(signal.attentionSoft)
                            .padding(start = 12.dp, end = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        AgentAvatar(name, request.userId, 28.dp)
                        Spacer(Modifier.width(10.dp))
                        Column(Modifier.weight(1f)) {
                            Text(name, style = MaterialTheme.typography.titleSmall, maxLines = 1)
                            Text(
                                "Demande l’accès" +
                                    if (request.mentions > 1) " · ${request.mentions} mentions"
                                    else "",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        TextButton(
                            onClick = { person(request.userId, "deny") },
                            enabled = !state.busy,
                        ) {
                            Text("Refuser")
                        }
                        TextButton(
                            onClick = { person(request.userId, "approve") },
                            enabled = !state.busy,
                        ) {
                            Text("Approuver")
                        }
                    }
                }
            }
            if (manifest.discord)
                Fact(
                    "Écoute dans",
                    "#${connection.setting("channelName") ?: connection.setting("channelId").orEmpty()}",
                    connection.setting("guildName") ?: "Serveur Discord",
                )
            else
                manifest.settings
                    .filter { !it.hidden }
                    .forEach { field ->
                        Fact(field.title, connection.setting(field.key) ?: "—")
                    }
            if (manifest.delegates) {
                Fact(
                    "Répond en tant que",
                    agent?.name ?: "Agent introuvable",
                    project?.name ?: "Aucun projet",
                )
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Row {
                        Text(
                            "Usage cette heure",
                            Modifier.weight(1f),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(
                            "${connection.usage.hour} / ${connection.maxMessagesPerHour} messages",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    LinearProgressIndicator(
                        progress = {
                            (connection.usage.hour.toFloat() /
                                    connection.maxMessagesPerHour.coerceAtLeast(1))
                                .coerceIn(0f, 1f)
                        },
                        modifier = Modifier.fillMaxWidth().height(6.dp).clip(CircleShape),
                        drawStopIndicator = {},
                    )
                }
                Eyebrow(
                    "Qui peut l’utiliser · " +
                        when (connection.accessMode) {
                            "everyone" ->
                                "tout le monde · ${connection.maxPerPersonPerHour ?: 5}/heure chacun"
                            "requests" -> "validation à la demande"
                            else -> "personnes listées"
                        }
                )
                if (connection.userIds.isEmpty() && connection.accessMode != "everyone")
                    Text(
                        if (connection.accessMode == "requests")
                            "Personne pour l’instant. Mentionnez le bot et approuvez votre propre demande ici."
                        else "Personne pour l’instant.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                connection.userIds.forEach { user ->
                    val name = connection.personName(user)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        AgentAvatar(name, user, 28.dp)
                        Spacer(Modifier.width(10.dp))
                        Column(Modifier.weight(1f)) {
                            Text(name, style = MaterialTheme.typography.bodyMedium, maxLines = 1)
                            Text(
                                user,
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        ActionIcon("Retirer $name", CairnIcons.Close, enabled = !state.busy) {
                            person(user, "remove")
                        }
                    }
                }
            }
            if (manifest.discord) {
                Eyebrow("Ce que ce bot peut faire")
                connection
                    .discordAbilities(
                        agent,
                        project?.name,
                        onePassword.filter { connection.agentId in it.agentIds }.map { it.name },
                    )
                    .forEach { Ability(it) }
                Eyebrow("Activité récente")
                if (connection.activity.isEmpty())
                    Text(
                        "Rien pour l’instant. Mentionnez @${connection.setting("botName") ?: "le bot"} dans #${connection.setting("channelName") ?: connection.setting("channelId").orEmpty()} pour l’essayer.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                connection.activity.forEach { item ->
                    ActivityRow(connection, item) { openChat(item.chatId) }
                }
            }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Toggle(
                if (manifest.discord) "Accepter les nouveaux messages" else "Active",
                connection.enabled,
            ) { enabled ->
                if (!state.busy)
                    vm.perform {
                        api.request(
                            "POST",
                            "$path/enabled",
                            buildJsonObject { put("enabled", enabled) },
                        )
                        changed()
                    }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                if (manifest.discord)
                    TextButton(
                        onClick = {
                            vm.perform {
                                val result = api.send<JsonObject>("POST", "$path/test")
                                notify(
                                    "Jeton valide · connecté en tant que ${result["name"]?.jsonPrimitive?.contentOrNull ?: connection.name}"
                                )
                            }
                        },
                        enabled = !state.busy,
                    ) {
                        Text("Tester")
                    }
                TextButton(onClick = edit, enabled = !state.busy) {
                    Text(if (manifest.discord) "Modifier sur le web" else "Modifier")
                }
                TextButton(onClick = remove, enabled = !state.busy) {
                    Text(
                        if (manifest.discord) "Retirer le bot" else "Supprimer",
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

@Composable
private fun ActivityRow(connection: PluginConnection, item: PluginActivity, open: () -> Unit) {
    val name = item.name.ifBlank { connection.personName(item.userId) }
    Surface(
        onClick = open,
        color = Color.Transparent,
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
    ) {
        Row(Modifier.padding(vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            AgentAvatar(name, item.userId, 28.dp)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(
                    "$name · ${shortStamp(item.createdAt)}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    "« ${item.preview} »",
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.width(8.dp))
            when (item.status) {
                "working" ->
                    Pill(
                        "En cours",
                        MaterialTheme.colorScheme.primaryContainer,
                        MaterialTheme.colorScheme.primary,
                    )
                "replied" -> Pill("Répondu", signal.success.copy(alpha = 0.14f), signal.success)
                "failed" -> Pill("Non livré", signal.attentionSoft, signal.attention)
                else ->
                    Pill(
                        "Arrêté",
                        MaterialTheme.colorScheme.surfaceVariant,
                        MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
        }
    }
}

/**
 * Generic instance settings: the manifest's fields, then the delegation for conversation plugins.
 */
@Composable
private fun InstanceEditor(
    vm: CairnViewModel,
    state: Workspace,
    pluginId: String,
    manifest: PluginManifest,
    initial: PluginConnection,
    close: () -> Unit,
    saved: suspend () -> Unit,
) {
    val creating = initial.id.isBlank()
    val fields = manifest.settings.filter { !it.hidden }
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    // Deliberately not saveable: never persist a credential in saved instance state.
    var token by remember(initial.id) { mutableStateOf("") }
    var values by
        remember(initial.id) {
            mutableStateOf(
                fields.associate { field ->
                    field.key to
                        when (field.type) {
                            "boolean" -> initial.flag(field.key).toString()
                            else -> initial.setting(field.key).orEmpty()
                        }
                }
            )
        }
    var agentId by remember(initial.id) { mutableStateOf(initial.agentId) }
    var accessMode by remember(initial.id) { mutableStateOf(initial.accessMode) }
    var userIds by remember(initial.id) { mutableStateOf(initial.userIds.joinToString(", ")) }
    var perHour by remember(initial.id) { mutableStateOf(initial.maxMessagesPerHour.toString()) }
    var enabled by remember(initial.id) { mutableStateOf(initial.enabled) }
    val agents = state.agents.filter { it.eligibleForPlugins() }
    Editor(
        if (creating) "Nouvelle instance ${manifest.name}" else "Modifier ${initial.name}",
        state.busy,
        state.error,
        close = close,
        valid =
            name.isNotBlank() &&
                (!creating || manifest.credential == null || token.isNotBlank()) &&
                fields.none {
                    it.required && it.type != "boolean" && values[it.key].isNullOrBlank()
                } &&
                (!manifest.delegates || agentId.isNotBlank()),
        save = {
            vm.perform {
                val ids = userIds.split(Regex("[\\s,]+")).filter { it.isNotBlank() }
                val agent = state.agents.find { it.id == agentId }
                api.request(
                    if (creating) "POST" else "PUT",
                    "/plugins/${segment(pluginId)}/connections" +
                        if (creating) "" else "/${segment(initial.id)}",
                    buildJsonObject {
                        put("name", name.trim())
                        if (token.isNotBlank()) put("token", token.trim())
                        put("settings", pluginSettings(fields, values))
                        if (manifest.delegates) {
                            put("agentId", agentId)
                            put(
                                "projectId",
                                agent?.access?.projects?.firstOrNull()?.let(::JsonPrimitive)
                                    ?: JsonNull,
                            )
                            put("accessMode", accessMode)
                            put("userIds", JsonArray(ids.map(::JsonPrimitive)))
                            put(
                                "people",
                                JsonArray(
                                    ids.map { user ->
                                        buildJsonObject {
                                            put("id", user)
                                            put(
                                                "name",
                                                initial.people
                                                    .find { it.id == user }
                                                    ?.name
                                                    .orEmpty(),
                                            )
                                        }
                                    }
                                ),
                            )
                            put("maxMessagesPerHour", perHour.toIntOrNull() ?: 20)
                        }
                        put("enabled", enabled)
                    },
                )
                token = ""
                notify("Instance enregistrée")
                saved()
            }
        },
    ) {
        Field("Nom", name, { name = it }, enabled = !state.busy)
        manifest.credential?.let { credential ->
            OutlinedTextField(
                token,
                { token = it },
                Modifier.fillMaxWidth(),
                label = { Text(credential.label) },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = InputKeyboards.Password,
                enabled = !state.busy,
                placeholder =
                    if (creating) null else ({ Text("Enregistré · laissez vide pour le garder") }),
                supportingText = {
                    Text(
                        "Chiffré. Cairn l’ajoute aux requêtes de ce plugin ; le plugin ne le voit jamais."
                    )
                },
            )
        }
        fields.forEach { field ->
            if (field.type == "boolean")
                Toggle(field.title, values[field.key] == "true") {
                    if (!state.busy) values = values + (field.key to it.toString())
                }
            else
                Column {
                    Field(
                        field.title,
                        values[field.key].orEmpty(),
                        { values = values + (field.key to it) },
                        enabled = !state.busy,
                        keyboardOptions =
                            if (field.type == "number")
                                KeyboardOptions(keyboardType = KeyboardType.Number)
                            else InputKeyboards.Sentences,
                    )
                    if (field.description.isNotBlank())
                        Text(
                            field.description,
                            Modifier.padding(start = 4.dp, top = 4.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
        }
        if (manifest.delegates) {
            Choice("Agent", agentId, agents.map { it.id to it.name }) { agentId = it }
            if (agents.isEmpty())
                Text(
                    "Aucun agent dédié : il faut un agent limité à un projet au plus, sans accès à l’installation.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            Choice(
                "Qui peut l’utiliser",
                accessMode,
                listOf(
                    "requests" to "Valider les personnes à leur demande",
                    "allowlist" to "Seulement les personnes listées",
                    "everyone" to
                        "Tout le monde (agent en lecture seule sans GitHub, MCP ni 1Password)",
                ),
            ) {
                accessMode = it
            }
            Field("Identifiants des personnes", userIds, { userIds = it }, enabled = !state.busy)
            Field(
                "Messages par heure",
                perHour,
                { perHour = it.filter(Char::isDigit).take(3) },
                enabled = !state.busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            )
        }
        Toggle("Active", enabled) { if (!state.busy) enabled = it }
    }
}

/** Review a package: what each version may do, then approve, switch version or uninstall. */
@Composable
private fun PluginReview(
    vm: CairnViewModel,
    state: Workspace,
    entry: PluginEntry,
    capabilities: List<PluginCapability>,
    close: () -> Unit,
    changed: suspend () -> Unit,
) {
    val valid = entry.versions.filter { it.manifest != null }
    // Review the newest version waiting for approval, otherwise the active one.
    var selected by
        remember(entry.id) {
            mutableStateOf(
                valid.lastOrNull { it.approved == false }?.version
                    ?: entry.active
                    ?: valid.lastOrNull()?.version.orEmpty()
            )
        }
    var uninstalling by remember { mutableStateOf(false) }
    val version = valid.find { it.version == selected }
    val manifest = version?.manifest
    val active = valid.find { it.version == entry.active }?.manifest
    val added =
        manifest?.capabilities.orEmpty().filter { active != null && it !in active.capabilities }
    fun act(operation: String) = vm.perform {
        api.request(
            "POST",
            "/plugins/packages/${segment(entry.id)}/$operation",
            buildJsonObject {
                put("version", selected)
                version?.digest?.let { put("digest", it) }
            },
        )
        notify("${manifest?.name ?: entry.name} $selected est actif")
        close()
        changed()
    }
    Dialog(
        onDismissRequest = { if (!state.busy) close() },
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Scaffold(
            modifier = Modifier.fillMaxSize().testTag("plugin-review"),
            topBar = {
                TopAppBar(
                    title = { Text("Examiner ${entry.name}") },
                    navigationIcon = {
                        IconButton(onClick = close, enabled = !state.busy) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer")
                        }
                    },
                )
            },
            bottomBar = {
                Row(
                    Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    if (!entry.builtin)
                        TextButton(
                            onClick = {
                                if (!uninstalling) uninstalling = true
                                else
                                    vm.perform {
                                        api.request(
                                            "DELETE",
                                            "/plugins/packages/${segment(entry.id)}",
                                        )
                                        notify("${entry.name} désinstallé")
                                        close()
                                        changed()
                                    }
                            },
                            enabled = !state.busy,
                        ) {
                            Text(
                                if (uninstalling) "Confirmer la désinstallation"
                                else "Désinstaller",
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                    Spacer(Modifier.weight(1f))
                    if (!entry.builtin && version != null && version.approved != true)
                        SignalButton(
                            "Approuver et activer",
                            height = 40.dp,
                            enabled = !state.busy,
                        ) {
                            act("approve")
                        }
                    else if (!entry.builtin && version != null && selected != entry.active)
                        SignalButton("Passer à $selected", height = 40.dp, enabled = !state.busy) {
                            act("activate")
                        }
                }
            },
        ) { padding ->
            Column(
                Modifier.padding(padding)
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                if (manifest != null)
                    Row(verticalAlignment = Alignment.Top) {
                        PluginMark(entry.id, 44.dp)
                        Spacer(Modifier.width(12.dp))
                        Column {
                            Text(
                                "${manifest.name} ${manifest.version}",
                                style = MaterialTheme.typography.titleMedium,
                            )
                            Text(
                                manifest.description.ifBlank { "Aucune description." },
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                if (valid.size > 1) {
                    Eyebrow("Versions")
                    valid.forEach { item ->
                        Surface(
                            onClick = { selected = item.version },
                            shape = RoundedCornerShape(14.dp),
                            color =
                                if (item.version == selected)
                                    MaterialTheme.colorScheme.primaryContainer
                                else MaterialTheme.colorScheme.surfaceContainer,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Row(
                                Modifier.padding(horizontal = 14.dp, vertical = 10.dp),
                                verticalAlignment = Alignment.CenterVertically,
                            ) {
                                RadioButton(item.version == selected, null)
                                Spacer(Modifier.width(8.dp))
                                Text(
                                    item.version,
                                    Modifier.weight(1f),
                                    fontWeight = FontWeight.SemiBold,
                                )
                                when {
                                    item.version == entry.active ->
                                        Pill(
                                            "Active",
                                            signal.success.copy(alpha = 0.14f),
                                            signal.success,
                                        )
                                    item.approved == true ->
                                        Text(
                                            "Approuvée",
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    else -> Pill("À approuver", signal.warningSoft, signal.warning)
                                }
                            }
                        }
                    }
                }
                entry.versions
                    .filter { it.error != null }
                    .forEach {
                        Text(
                            "La version ${it.version} est inutilisable : ${it.error}",
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                if (manifest != null) {
                    Eyebrow("Ce qu’il peut faire")
                    if (manifest.capabilities.isEmpty())
                        Text(
                            "Aucun accès à Cairn ni au réseau.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    manifest.capabilities.forEach { name ->
                        Surface(
                            shape = RoundedCornerShape(14.dp),
                            color = MaterialTheme.colorScheme.surfaceContainer,
                            modifier = Modifier.fillMaxWidth(),
                        ) {
                            Column(Modifier.padding(horizontal = 14.dp, vertical = 10.dp)) {
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Text(
                                        capabilityLabel(name),
                                        style = MaterialTheme.typography.titleSmall,
                                    )
                                    if (name in added) {
                                        Spacer(Modifier.width(8.dp))
                                        Pill("Nouveau", signal.warningSoft, signal.warning)
                                    }
                                }
                                capabilities
                                    .find { it.name == name }
                                    ?.description
                                    ?.let {
                                        Text(
                                            it,
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    }
                            }
                        }
                    }
                    if (manifest.hosts.isNotEmpty())
                        Note(
                            "Ne peut joindre que ${manifest.hosts.joinToString(", ")}, en HTTPS, sans suivre les redirections."
                        )
                    manifest.credential?.let {
                        Note(
                            "Cairn ajoute le ${it.label} de chaque instance à ses requêtes (en-tête ${it.header}). Le plugin ne le voit jamais."
                        )
                    }
                    Note(
                        "S’exécute dans le bac à sable Luau de Cairn : ni fichiers, ni processus, ni autre accès réseau. Toute modification sur disque demande une nouvelle validation."
                    )
                }
            }
        }
    }
}

@Composable
private fun Note(text: String) =
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )

@Composable
private fun PluginHeader(
    id: String,
    name: String,
    builtin: Boolean,
    version: String?,
    description: String,
    capabilities: List<String>,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            PluginMark(id, 56.dp)
            Spacer(Modifier.width(14.dp))
            Column {
                Text(name, style = MaterialTheme.typography.headlineMedium)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Pill(
                        if (builtin) "Intégré" else "Installé",
                        MaterialTheme.colorScheme.primaryContainer,
                        MaterialTheme.colorScheme.primary,
                    )
                    if (version != null)
                        Text(
                            "  v$version · bac à sable Luau",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
            }
        }
        if (description.isNotBlank())
            Text(
                description,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            capabilities.forEach {
                Pill(
                    it,
                    MaterialTheme.colorScheme.surfaceVariant,
                    MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun Fact(title: String, value: String, detail: String? = null) {
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .padding(horizontal = 14.dp, vertical = 10.dp)
    ) {
        Eyebrow(title)
        Text(
            value,
            Modifier.padding(top = 2.dp),
            style = MaterialTheme.typography.titleSmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        if (detail != null)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
    }
}

@Composable
private fun Ability(ability: PluginAbility) {
    Row(Modifier.padding(vertical = 3.dp), verticalAlignment = Alignment.Top) {
        Icon(
            when {
                !ability.allowed -> CairnIcons.Close
                ability.warn -> CairnIcons.Alert
                else -> CairnIcons.Check
            },
            null,
            Modifier.padding(top = 1.dp).size(16.dp),
            tint =
                when {
                    !ability.allowed -> MaterialTheme.colorScheme.outline
                    ability.warn -> signal.warning
                    else -> signal.success
                },
        )
        Spacer(Modifier.width(10.dp))
        Text(
            ability.text,
            style = MaterialTheme.typography.bodySmall,
            color =
                if (ability.allowed) MaterialTheme.colorScheme.onSurface
                else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun Pill(text: String, container: Color, content: Color) {
    Text(
        text,
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        style = MaterialTheme.typography.labelMedium,
        fontWeight = FontWeight.SemiBold,
        color = content,
        maxLines = 1,
    )
}

@Composable
internal fun ToneChip(badge: PluginBadge) {
    val (container, content) =
        when (badge.tone) {
            PluginTone.OK -> signal.success.copy(alpha = 0.14f) to signal.success
            PluginTone.ATTENTION -> signal.attentionSoft to signal.attention
            PluginTone.WARNING -> signal.warningSoft to signal.warning
            PluginTone.MUTED ->
                MaterialTheme.colorScheme.surfaceVariant to
                    MaterialTheme.colorScheme.onSurfaceVariant
        }
    Row(
        Modifier.clip(CircleShape)
            .background(container)
            .padding(horizontal = 9.dp, vertical = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(6.dp).background(content, CircleShape))
        Spacer(Modifier.width(5.dp))
        Text(
            badge.label,
            style = MaterialTheme.typography.labelMedium,
            fontWeight = FontWeight.SemiBold,
            color = content,
            maxLines = 1,
        )
    }
}

/** The plugin's brand on its colour, or a puzzle piece for packages without one. */
@Composable
internal fun PluginMark(id: String, size: Dp) {
    val brand =
        when (id) {
            "discord" -> DiscordMark to Color(0xFF5865F2)
            "onepassword" -> OnePasswordMark to Color(0xFF1A285F)
            else -> null
        }
    Box(
        Modifier.size(size)
            .clip(RoundedCornerShape(size * 0.29f))
            .background(brand?.second ?: MaterialTheme.colorScheme.primaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            brand?.first ?: PuzzleMark,
            null,
            Modifier.size(size * 0.52f),
            tint = if (brand != null) Color.White else MaterialTheme.colorScheme.primary,
        )
    }
}

private fun filledMark(name: String, path: String) =
    ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        .addPath(addPathNodes(path), fill = SolidColor(Color.Black))
        .build()

private val DiscordMark by lazy {
    filledMark(
        "Discord",
        "M20.317 4.37a19.8 19.8 0 0 0-4.885-1.515a.074.074 0 0 0-.079.037c-.21.375-.444.864-.608 1.25a18.3 18.3 0 0 0-5.487 0a13 13 0 0 0-.617-1.25a.08.08 0 0 0-.079-.037A19.7 19.7 0 0 0 3.677 4.37a.1.1 0 0 0-.032.027C.533 9.046-.32 13.58.099 18.057a.08.08 0 0 0 .031.057a19.9 19.9 0 0 0 5.993 3.03a.08.08 0 0 0 .084-.028a14 14 0 0 0 1.226-1.994a.076.076 0 0 0-.041-.106a13 13 0 0 1-1.872-.892a.077.077 0 0 1-.008-.128a10 10 0 0 0 .372-.292a.07.07 0 0 1 .077-.01c3.928 1.793 8.18 1.793 12.062 0a.07.07 0 0 1 .078.01q.181.149.373.292a.077.077 0 0 1-.006.127a12.3 12.3 0 0 1-1.873.892a.077.077 0 0 0-.041.107c.36.698.772 1.362 1.225 1.993a.08.08 0 0 0 .084.028a19.8 19.8 0 0 0 6.002-3.03a.08.08 0 0 0 .032-.054c.5-5.177-.838-9.674-3.549-13.66a.06.06 0 0 0-.031-.03M8.02 15.33c-1.182 0-2.157-1.085-2.157-2.419c0-1.333.956-2.419 2.157-2.419c1.21 0 2.176 1.096 2.157 2.42c0 1.333-.956 2.418-2.157 2.418m7.975 0c-1.183 0-2.157-1.085-2.157-2.419c0-1.333.955-2.419 2.157-2.419c1.21 0 2.176 1.096 2.157 2.42c0 1.333-.946 2.418-2.157 2.418",
    )
}

private val OnePasswordMark by lazy {
    filledMark(
        "1Password",
        "M12 0c6.627 0 12 5.373 12 12c0 6.628-5.373 12-12 12S0 18.628 0 12C0 5.373 5.373 0 12 0m-.893 4.86c-.485 0-.727.001-.913.095a.87.87 0 0 0-.378.379c-.094.185-.095.428-.095.912v2.747c0 .12 0 .182.016.238q.02.075.065.138a1 1 0 0 0 .175.162l.695.564c.113.092.17.139.19.194a.22.22 0 0 1 0 .15c-.02.056-.077.102-.19.194l-.695.564a1 1 0 0 0-.175.162a.4.4 0 0 0-.065.138a1 1 0 0 0-.016.238v6.019c0 .485 0 .728.095.913a.87.87 0 0 0 .378.378c.186.094.428.094.913.094h1.786c.485 0 .727 0 .913-.094a.87.87 0 0 0 .378-.378c.095-.185.095-.428.095-.913v-2.747c0-.12 0-.182-.016-.238a.4.4 0 0 0-.065-.138a1 1 0 0 0-.175-.162l-.695-.564c-.113-.092-.17-.138-.191-.193a.22.22 0 0 1 0-.152c.02-.055.078-.1.19-.193l.696-.564a1 1 0 0 0 .175-.162a.4.4 0 0 0 .065-.138a1 1 0 0 0 .016-.238V6.246c0-.484 0-.727-.095-.912a.87.87 0 0 0-.378-.379c-.186-.094-.428-.094-.913-.094Z",
    )
}

private val PuzzleMark by lazy {
    ImageVector.Builder("Puzzle", 24.dp, 24.dp, 24f, 24f)
        .addPath(
            addPathNodes(
                "M15.39 4.39a1 1 0 0 0 1.68-.474a2.5 2.5 0 1 1 3.014 3.015a1 1 0 0 0-.474 1.68l1.683 1.682a2.414 2.414 0 0 1 0 3.414L19.61 15.39a1 1 0 0 1-1.68-.474a2.5 2.5 0 1 0-3.014 3.015a1 1 0 0 1 .474 1.68l-1.683 1.682a2.414 2.414 0 0 1-3.414 0L8.61 19.61a1 1 0 0 0-1.68.474a2.5 2.5 0 1 1-3.014-3.015a1 1 0 0 0 .474-1.68l-1.683-1.682a2.414 2.414 0 0 1 0-3.414L4.39 8.61a1 1 0 0 1 1.68.474a2.5 2.5 0 1 0 3.014-3.015a1 1 0 0 1-.474-1.68l1.683-1.682a2.414 2.414 0 0 1 3.414 0z"
            ),
            fill = null,
            stroke = SolidColor(Color.Black),
            strokeLineWidth = 1.8f,
            strokeLineCap = StrokeCap.Round,
            strokeLineJoin = StrokeJoin.Round,
        )
        .build()
}
