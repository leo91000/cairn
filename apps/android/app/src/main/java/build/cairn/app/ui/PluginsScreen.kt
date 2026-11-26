@file:OptIn(
    ExperimentalLayoutApi::class,
    androidx.compose.material3.ExperimentalMaterial3Api::class,
)

package build.cairn.app.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.ListPluginPackagesInput
import build.cairn.contracts.installPluginPackage
import build.cairn.contracts.listPluginPackages
import kotlinx.serialization.json.*

/** Resolve an authenticated installation page for external links. */
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
    val context = LocalContext.current
    val upload =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { folder ->
            if (folder != null)
                vm.perform {
                    val version = api.installPluginPackage(readPluginPackage(context, folder))
                    catalog = api.listPluginPackages(ListPluginPackagesInput())
                    notify(
                        "${version.manifest?.name ?: version.version} installé · examinez-le avant activation"
                    )
                }
        }
    Poll("plugins", 10_000) {
        try {
            catalog = vm.api.listPluginPackages(ListPluginPackagesInput())
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
                PluginCard(entry, entry.summary(current.connections)) {
                    vm.clearMessage()
                    open(entry.id)
                }
            }
        SignalCard(Modifier.fillMaxWidth().padding(top = 8.dp)) {
            Text("Installer un package", style = MaterialTheme.typography.titleSmall)
            Text(
                "Choisissez le dossier contenant manifest.json ou manifest.toml et main.luau, puis examinez le package avant de l’activer.",
                Modifier.padding(top = 4.dp, bottom = 12.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            SignalButton(
                "Choisir le dossier",
                icon = CairnIcons.External,
                container = MaterialTheme.colorScheme.surface,
                content = MaterialTheme.colorScheme.onSurface,
                border = true,
                height = 40.dp,
            ) {
                upload.launch(null)
            }
        }
    }
}

@Composable
private fun PluginCard(entry: PluginEntry, summary: PluginSummary, open: () -> Unit) {
    SignalCard(Modifier.fillMaxWidth().testTag("plugin-${entry.id}"), onClick = open) {
        Row(verticalAlignment = Alignment.Top) {
            PluginMark(entry.id, 44.dp, entry.manifest?.brand)
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
            // What connecting takes, until the first instance exists.
            if (summary.effort != null) {
                Spacer(Modifier.width(6.dp))
                EffortPill(summary.effort)
            }
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

/** One plugin: its instances, what it gives agents, and its review. */
@Composable
fun PluginScreen(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openAgents: () -> Unit = {},
    openRules: (String) -> Unit = {},
    openChat: (String) -> Unit,
) = PluginInstances(vm, state, id, openChat, openAgents, openRules)

@Composable
private fun PluginInstances(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openChat: (String) -> Unit,
    openAgents: () -> Unit,
    openRules: (String) -> Unit,
) {
    var catalog by remember { mutableStateOf<PluginCatalog?>(null) }
    var editing by remember { mutableStateOf<PluginConnection?>(null) }
    var removing by remember { mutableStateOf<PluginConnection?>(null) }
    var reviewing by remember { mutableStateOf(false) }
    val context = LocalContext.current
    suspend fun load() {
        catalog = vm.api.listPluginPackages(ListPluginPackagesInput())
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
            manifest.capabilities.map(::capabilityLabel) + manifest.contributions(),
            manifest.brand,
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
                SignalButton(
                    if (manifest.instances != null) "Ajouter · ${manifest.noun()}"
                    else "Ajouter une instance",
                    icon = CairnIcons.Plus,
                    height = 40.dp,
                ) {
                    vm.clearMessage()
                    editing =
                        PluginConnection(
                            name = if (manifest.instances == null) manifest.name else "",
                            accessMode = "requests",
                            settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                            status = PluginStatus(),
                            usage = PluginUsage(),
                            enabled = true,
                        )
                }
        }
        if (instances.isEmpty() && entry.active != null)
            FirstSetup(manifest, entry.resolvedSetup()) {
                vm.clearMessage()
                editing =
                    PluginConnection(
                        name = if (manifest.instances == null) manifest.name else "",
                        accessMode = "requests",
                        settings = kotlinx.serialization.json.JsonObject(emptyMap()),
                        status = PluginStatus(),
                        usage = PluginUsage(),
                        enabled = true,
                    )
            }
        instances.forEach { connection ->
            InstanceCard(
                vm,
                state,
                manifest,
                connection,
                catalog?.grantsOf(connection.agentId).orEmpty(),
                openChat = openChat,
                openAgents = openAgents,
                openRules = { openRules(connection.id) },
                edit = {
                    vm.clearMessage()
                    editing = connection
                },
                remove = {
                    vm.clearMessage()
                    removing = connection
                },
                changed = { load() },
            )
        }
        if (entry.active != null && manifest.programs.any { it.installer.isNotBlank() })
            PluginPrograms(vm, state, manifest.id)
        if (manifest.agentAccess != null) AgentUsage(manifest)
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
            InstanceEditor(
                vm,
                state,
                id,
                requireNotNull(entry),
                it,
                initial,
                catalog?.connections.orEmpty(),
                close = { editing = null },
                openAgents = {
                    editing = null
                    openAgents()
                },
            ) {
                editing = null
                load()
            }
        }
    }
    removing?.let { connection ->
        Confirm(
            "Supprimer ${connection.name} ?",
            "L’instance s’arrête ; son identifiant, ses valeurs enregistrées et ses minuteurs sont supprimés. Les conversations restent dans Cairn.",
            state.busy,
            state.error,
            dismiss = { removing = null },
        ) {
            vm.perform {
                api.deletePlugin(id, connection.id)
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
    /** Other plugins the delegated agent is authorized on, for what it can do. */
    grants: List<String>,
    openChat: (String) -> Unit,
    openAgents: () -> Unit,
    openRules: () -> Unit,
    edit: () -> Unit,
    remove: () -> Unit,
    changed: suspend () -> Unit,
) {
    val agent = state.agents.find { it.id == connection.agentId }
    val project = state.projects.find { it.id == connection.projectId }
    val badge = connection.state()
    fun person(userId: String, action: String) = vm.perform {
        api.pluginPerson(connection.pluginId, connection.id, userId, action)
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
                    // The package's own words, e.g. who it is signed in as.
                    connection.status.message
                        ?.takeIf { it.isNotBlank() && connection.status.state != "needs_review" }
                        ?.let {
                            Text(
                                it,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                }
                ToneChip(badge)
            }
            if (connection.enabled && connection.status.state == "needs_review")
                Text(
                    connection.status.message
                        ?.takeIf { it.isNotBlank() }
                        ?.let {
                            "$it Modifiez l’instance pour la vérifier."
                        } ?: "Cairn a arrêté cette instance. Modifiez-la pour la vérifier.",
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
            // Packages with their own panel describe the instance there; secrets show as set.
            manifest.settingFields
                .filter { !it.hidden && (!manifest.instanceView || it.type == "secret") }
                .forEach { field ->
                    if (field.type == "secret")
                        Fact(
                            field.title,
                            if (field.key in connection.secretKeys) "Enregistré" else "Non défini",
                        )
                    else Fact(field.title, connection.display(field.key))
                }
            TextButton(onClick = openRules) { Text("Règles utilisant cette instance ›") }
            if (manifest.agentAccess != null) {
                val names =
                    connection.agentIds.map { id ->
                        state.agents.find { it.id == id }?.name ?: "Agent supprimé"
                    }
                Text(
                    if (names.isEmpty()) "Aucun agent ne peut l’utiliser pour l’instant."
                    else "Agents autorisés : ${names.joinToString(", ")}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                TextButton(onClick = openAgents) { Text("Gérer sur les fiches agents ›") }
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
                            "Personne pour l’instant. Écrivez-lui et approuvez votre propre demande ici."
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
            if (manifest.delegates) {
                Eyebrow("Ce qu’on peut lui faire faire")
                connection.abilities(agent, project?.name, grants).forEach { Ability(it) }
                Eyebrow("Activité récente")
                if (connection.activity.isEmpty())
                    Text(
                        "Rien pour l’instant.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                connection.activity.forEach { item ->
                    ActivityRow(connection, item) { openChat(item.chatId) }
                }
            }
            if (manifest.instanceView)
                PluginPanel(vm, state, connection.pluginId, connection.id, connection.name)
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            Toggle(
                if (manifest.delegates) "Accepter les nouveaux messages" else "Active",
                connection.enabled,
            ) { enabled ->
                if (!state.busy)
                    vm.perform {
                        api.enablePlugin(connection.pluginId, connection.id, enabled)
                        changed()
                    }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                if (manifest.credential?.oauth != null)
                    TextButton(
                        onClick = {
                            vm.perform {
                                notify(api.disconnectPlugin(connection.pluginId, connection.id))
                                changed()
                            }
                        },
                        enabled = !state.busy,
                    ) {
                        Text("Déconnecter")
                    }
                if (manifest.instanceTest)
                    TextButton(
                        onClick = {
                            vm.perform {
                                val message = api.testPlugin(connection.pluginId, connection.id)
                                notify(
                                    "${connection.name} : ${message.ifBlank { "ça fonctionne" }}"
                                )
                            }
                        },
                        enabled = !state.busy,
                    ) {
                        Text("Tester")
                    }
                TextButton(onClick = edit, enabled = !state.busy) { Text("Modifier") }
                TextButton(onClick = remove, enabled = !state.busy) {
                    Text("Supprimer", color = MaterialTheme.colorScheme.error)
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
