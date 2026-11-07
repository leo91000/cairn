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
import androidx.compose.foundation.selection.toggleable
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
import androidx.compose.ui.semantics.Role
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
import build.cairn.contracts.ActivatePluginPackageInput
import build.cairn.contracts.ApprovePluginPackageInput
import build.cairn.contracts.ListPluginPackagesInput
import build.cairn.contracts.activatePluginPackage
import build.cairn.contracts.approvePluginPackage
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
    openChat: (String) -> Unit,
) = PluginInstances(vm, state, id, openChat, openAgents)

@Composable
private fun PluginInstances(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openChat: (String) -> Unit,
    openAgents: () -> Unit,
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

@Composable
internal fun FormError(text: String) {
    Text(
        text,
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(12.dp))
            .background(signal.attentionSoft)
            .padding(12.dp),
        style = MaterialTheme.typography.bodySmall,
        color = signal.attention,
    )
}

/**
 * Signing in on the service's page instead of pasting a token: the code to enter, copyable, and the
 * page in a Custom Tab, until the installation holds the credential.
 */
@Composable
internal fun DeviceSignIn(
    manifest: PluginManifest,
    flow: PluginDeviceFlow?,
    error: String?,
    /** The button starting a first sign-in, e.g. the step's title. */
    label: String,
    /** Saving comes next, on this same step. */
    last: Boolean,
    enabled: Boolean,
    start: () -> Unit,
    cancel: () -> Unit,
) {
    val problem = error ?: signInProblem(flow)
    Column(
        Modifier.fillMaxWidth().testTag("plugin-device-sign-in"),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (flow == null || flow.completed)
            OutlinedButton(onClick = start, enabled = enabled) {
                Icon(CairnIcons.Key, null, Modifier.size(16.dp))
                Spacer(Modifier.width(8.dp))
                Text(if (flow?.completed == true) "Se connecter avec un autre compte" else label)
            }
        problem?.let { FormError(it) }
        if (flow?.completed == true)
            Text(
                if (last) "Connecté. Enregistrez pour garder ce ${manifest.noun().lowercase()}."
                else "Connecté.",
                style = MaterialTheme.typography.bodySmall,
                fontWeight = FontWeight.SemiBold,
                color = signal.success,
            )
        else if (flow?.pending == true)
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceContainer,
            ) {
                Column(
                    Modifier.fillMaxWidth().padding(14.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        if (manifest.credential?.oauth != null)
                            "Ouvrez ${manifest.name}, autorisez l’accès puis revenez ici pour enregistrer le compte. Votre mot de passe reste chez ${manifest.name}."
                        else
                            "Ouvrez ${manifest.name} et saisissez ce code. Votre mot de passe reste chez ${manifest.name}.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    if (flow.userCode.isNotBlank())
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            androidx.compose.foundation.text.selection.SelectionContainer(
                                Modifier.weight(1f, fill = false)
                            ) {
                                Text(
                                    flow.userCode,
                                    Modifier.semantics {
                                        contentDescription = "Code de vérification"
                                    },
                                    fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace,
                                    fontWeight = FontWeight.Bold,
                                    style = MaterialTheme.typography.headlineSmall,
                                )
                            }
                            Spacer(Modifier.width(8.dp))
                            CopyButton("Copier le code", flow.userCode)
                        }
                    flow.message
                        ?.takeIf { it.isNotBlank() }
                        ?.let {
                            Text(
                                it,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (flow.link.isNotBlank())
                            ExternalButton("Ouvrir ${manifest.name}", flow.link)
                        TextButton(onClick = cancel) { Text("Annuler la connexion") }
                    }
                }
            }
        else if (flow != null && signInProblem(flow) != null)
            TextButton(onClick = start, enabled = enabled) { Text("Réessayer") }
    }
}

/** Choices that explain themselves, e.g. why one can't be picked: one row each. */
@Composable
internal fun ChoiceList(
    label: String,
    selected: String,
    options: List<PluginOption>,
    enabled: Boolean = true,
    onChange: (String) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        if (options.isEmpty())
            Text(
                "Rien à choisir pour l’instant.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        options.forEach { option ->
            val chosen = option.value == selected
            Surface(
                onClick = { onChange(option.value) },
                enabled = enabled && !option.disabled,
                shape = RoundedCornerShape(12.dp),
                color =
                    if (chosen) MaterialTheme.colorScheme.primaryContainer
                    else MaterialTheme.colorScheme.surface,
                border =
                    androidx.compose.foundation.BorderStroke(
                        1.dp,
                        if (chosen) MaterialTheme.colorScheme.primary
                        else MaterialTheme.colorScheme.outlineVariant,
                    ),
                modifier =
                    Modifier.fillMaxWidth().semantics {
                        contentDescription =
                            option.label +
                                if (option.description.isNotBlank()) " · ${option.description}"
                                else ""
                    },
            ) {
                Row(
                    Modifier.padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(chosen, null, enabled = enabled && !option.disabled)
                    Spacer(Modifier.width(8.dp))
                    Column(Modifier.weight(1f)) {
                        Text(
                            option.label,
                            style = MaterialTheme.typography.bodyMedium,
                            color =
                                if (option.disabled) MaterialTheme.colorScheme.onSurfaceVariant
                                else MaterialTheme.colorScheme.onSurface,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (option.description.isNotBlank())
                            Text(
                                option.description,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                }
            }
        }
    }
}

/**
 * A package's declarative fields. Secret values live in their own map and are never prefilled: a
 * saved secret only shows as a placeholder and stays when left blank. Dynamic choices and errors
 * come from the package's form handler; fields marked `refresh` ask it again when they change.
 */
@Composable
internal fun PluginFields(
    fields: List<PluginSetting>,
    values: Map<String, String>,
    change: (String, String) -> Unit,
    secrets: Map<String, String>,
    changeSecret: (String, String) -> Unit,
    saved: List<String> = emptyList(),
    enabled: Boolean = true,
    options: Map<String, List<PluginOption>> = emptyMap(),
    errors: Map<String, String> = emptyMap(),
    refresh: (String) -> Unit = {},
) {
    var section = ""
    fields
        .filter { !it.hidden }
        .forEach { field ->
            fun update(value: String) {
                change(field.key, value)
                if (field.refresh) refresh(field.key)
            }
            val choices = (if (field.dynamic) options[field.key] else null) ?: field.options
            if (field.section.isNotBlank() && field.section != section) {
                section = field.section
                Eyebrow(field.section, Modifier.padding(top = 4.dp))
            }
            when (field.type) {
                "boolean" ->
                    if (field.risk == null)
                        Toggle(field.title, values[field.key] == "true") {
                            if (enabled) update(it.toString())
                        }
                    else
                        RiskToggle(field, values[field.key] == "true") {
                            if (enabled) update(it.toString())
                        }
                "select" ->
                    // Dynamic and self-explaining choices read better as a list.
                    if (field.dynamic || choices.any { it.description.isNotBlank() || it.disabled })
                        ChoiceList(field.title, values[field.key].orEmpty(), choices, enabled) {
                            update(it)
                        }
                    else
                        Choice(
                            field.title,
                            values[field.key].orEmpty(),
                            choices.map { it.value to it.label },
                        ) {
                            update(it)
                        }
                "multiselect" -> {
                    val chosen =
                        multiselect(values[field.key].orEmpty()).mapNotNull {
                            it.jsonPrimitive.contentOrNull
                        }
                    Text(field.title, style = MaterialTheme.typography.labelLarge)
                    choices.forEach { option ->
                        Toggle(option.label, option.value in chosen) { checked ->
                            if (enabled && !option.disabled) {
                                val next =
                                    if (checked) chosen + option.value else chosen - option.value
                                update(JsonArray(next.map(::JsonPrimitive)).toString())
                            }
                        }
                    }
                }
                "secret" ->
                    OutlinedTextField(
                        secrets[field.key].orEmpty(),
                        {
                            changeSecret(field.key, it)
                            if (field.refresh) refresh(field.key)
                        },
                        Modifier.fillMaxWidth(),
                        label = { Text(field.title) },
                        singleLine = true,
                        visualTransformation = PasswordVisualTransformation(),
                        keyboardOptions = InputKeyboards.Password,
                        enabled = enabled,
                        isError = field.key in errors,
                        placeholder = {
                            Text(
                                if (field.key in saved) "Enregistré · laissez vide pour le garder"
                                else field.placeholder
                            )
                        },
                    )
                else ->
                    Field(
                        field.title,
                        values[field.key].orEmpty(),
                        { update(it) },
                        lines = if (field.type == "text") 4 else 1,
                        enabled = enabled,
                        keyboardOptions =
                            when (field.type) {
                                "number" -> KeyboardOptions(keyboardType = KeyboardType.Number)
                                "url" -> KeyboardOptions(keyboardType = KeyboardType.Uri)
                                else -> InputKeyboards.Sentences
                            },
                    )
            }
            val note = errors[field.key] ?: field.description.takeIf { it.isNotBlank() }
            if (note != null)
                Text(
                    note,
                    Modifier.padding(start = 4.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color =
                        if (field.key in errors) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
}

/** A switch with what it lets agents do, e.g. « Risque élevé » for deleting mail. */
@Composable
private fun RiskToggle(field: PluginSetting, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 48.dp)
            .toggleable(value = checked, role = Role.Switch, onValueChange = onChange),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        FlowRow(
            Modifier.weight(1f),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
            itemVerticalAlignment = Alignment.CenterVertically,
        ) {
            Text(field.title)
            RiskPill(field.risk)
        }
        Switch(checked, null)
    }
}

/** What agents get from a package: its declared commands, tools and programs. */
@Composable
private fun AgentUsage(manifest: PluginManifest) {
    SignalCard(Modifier.fillMaxWidth().testTag("plugin-agent-usage")) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Icon(
                CairnIcons.Terminal,
                null,
                Modifier.size(18.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(8.dp))
            Text("Utilisation par les agents", style = MaterialTheme.typography.titleSmall)
        }
        Text(
            "Seuls les agents autorisés sur une instance active y ont accès, dans leurs conversations.",
            Modifier.padding(top = 6.dp, bottom = 4.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (manifest.commands.isNotEmpty()) {
            Eyebrow("Commandes", Modifier.padding(top = 8.dp, bottom = 6.dp))
            manifest.commands.forEach { command ->
                Surface(
                    shape = RoundedCornerShape(12.dp),
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    modifier = Modifier.fillMaxWidth().padding(bottom = 4.dp),
                ) {
                    Box(Modifier.padding(10.dp)) { Code(command.usage(manifest.id)) }
                }
                if (command.description.isNotBlank())
                    Note(
                        command.description +
                            if (command.output == "secret")
                                " La valeur va seulement dans la sortie de la commande, jamais dans la conversation."
                            else ""
                    )
                Spacer(Modifier.height(6.dp))
            }
        }
        if (manifest.tools.isNotEmpty()) {
            Eyebrow("Outils", Modifier.padding(top = 8.dp, bottom = 4.dp))
            manifest.tools.forEach { Code(it.name) }
        }
        if (manifest.programs.isNotEmpty()) {
            Eyebrow("Programmes", Modifier.padding(top = 10.dp, bottom = 4.dp))
            manifest.programs.forEach { program ->
                Code(program.path.ifBlank { program.name })
                if (program.description.isNotBlank()) Note(program.description)
            }
        }
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
        if (operation == "approve")
            api.approvePluginPackage(
                ApprovePluginPackageInput(requireNotNull(version?.digest), entry.id, selected)
            )
        else api.activatePluginPackage(ActivatePluginPackageInput(entry.id, selected))
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
                Column {
                    TopAppBar(
                        title = { Text("Examiner ${entry.name}") },
                        navigationIcon = {
                            IconButton(onClick = close, enabled = !state.busy) {
                                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer")
                            }
                        },
                    )
                    Column(Modifier.padding(horizontal = 20.dp)) { ConnectionFeedbackNotice() }
                }
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
                    manifest.programs.forEach { program ->
                        Text(
                            "Exécute ${program.path.ifBlank { program.name }} sur ce serveur, hors du bac à sable Luau, avec les fichiers et le réseau que ce programme peut atteindre." +
                                (if (program.installer.isNotBlank())
                                    " Installation et mises à jour par ${program.installer}, depuis les hôtes déclarés."
                                else "") +
                                program.description
                                    .takeIf { it.isNotBlank() }
                                    ?.let { " $it" }
                                    .orEmpty(),
                            Modifier.clip(RoundedCornerShape(12.dp))
                                .background(signal.warningSoft)
                                .padding(10.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = signal.warning,
                        )
                    }
                    if (manifest.agentAccess != null)
                        Note(
                            "Les agents autorisés sur une instance obtiennent ${manifest.commands.size} commande(s) et ${manifest.tools.size} outil(s)" +
                                (if (manifest.instructions.isNotBlank())
                                    ", et des instructions dans leur prompt"
                                else "") +
                                ". Rien n’est accordé par défaut."
                        )
                    Note(
                        "Le script s’exécute dans le bac à sable Luau de Cairn : ni fichiers, ni processus, ni autre accès réseau de lui-même. Toute modification d’un de ses fichiers sur disque demande une nouvelle validation."
                    )
                    version
                        ?.files
                        ?.takeIf { it.isNotEmpty() }
                        ?.let { files ->
                            Text(
                                files.joinToString("\n") {
                                    "${it.path} · ${maxOf(1, (it.bytes + 1023) / 1024)} Ko"
                                },
                                Modifier.testTag("plugin-files"),
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                }
            }
        }
    }
}

@Composable
internal fun Note(text: String) =
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
    brand: PluginBrand? = null,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            PluginMark(id, 56.dp, brand)
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
internal fun Pill(text: String, container: Color, content: Color) {
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
internal fun PluginMark(id: String, size: Dp, brand: PluginBrand? = null) {
    // The mark the package declares, or a puzzle piece.
    val declared =
        remember(brand) {
            brand?.let { mark ->
                val color = runCatching {
                    Color(android.graphics.Color.parseColor(mark.color))
                }
                    .getOrNull()
                val path =
                    mark.path
                        .takeIf { it.isNotBlank() }
                        ?.let { runCatching { filledMark(id, it) }.getOrNull() }
                color?.let { (path ?: PuzzleMark) to it }
            }
        }
    val known = declared
    Box(
        Modifier.size(size)
            .clip(RoundedCornerShape(size * 0.29f))
            .background(known?.second ?: MaterialTheme.colorScheme.primaryContainer),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            known?.first ?: PuzzleMark,
            null,
            Modifier.size(size * 0.52f),
            tint = if (known != null) Color.White else MaterialTheme.colorScheme.primary,
        )
    }
}

private fun filledMark(name: String, path: String) =
    ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        .addPath(addPathNodes(path), fill = SolidColor(Color.Black))
        .build()

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
