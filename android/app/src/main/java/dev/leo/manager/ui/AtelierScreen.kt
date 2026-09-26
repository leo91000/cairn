package dev.leo.manager.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import kotlinx.coroutines.CancellationException

/** What the Atelier shows about coding-agent accounts. */
internal data class ConnectionSummary(
    /** Coding agents that at least one agent runs on. */
    val used: Set<String>,
    val view: AccountsView?,
) {
    val accounts
        get() = view?.accounts

    fun ready(provider: String) =
        accounts.orEmpty().filter { it.provider == provider && it.enabled && it.state == "ready" }

    fun total(provider: String) = accounts.orEmpty().count { it.provider == provider }

    fun remaining(provider: String): Double? = ready(provider).mapNotNull { it.remainingPercent }.maxOrNull()

    /** Coding agents in use whose runs wait for the user to connect, reconnect or resume an account. */
    val missing: List<String>
        get() = view?.required.orEmpty().filter { it in used }
}

private suspend fun <T> optional(vm: LeoViewModel, load: suspend () -> T): T? =
    try {
        load()
    } catch (e: Exception) {
        if (e is CancellationException) throw e
        // A signed-out session must still return to the login screen.
        if (e is ApiException && e.status == 401) vm.report(e)
        null
    }

/** Atelier: what the agents can use, with health first and one tap to each resource. */
@Composable
fun AtelierScreen(vm: LeoViewModel, state: Workspace, navigate: (String) -> Unit) {
    var accounts by remember { mutableStateOf<AccountsView?>(null) }
    var onePassword by remember { mutableStateOf<List<OnePasswordAccount>?>(null) }
    var loaded by remember { mutableStateOf(false) }
    var signingOut by remember { mutableStateOf(false) }
    Poll("atelier", 30_000) {
        accounts = optional(vm) { vm.api.get<AccountsView>("/accounts") }
        onePassword = optional(vm) { vm.api.get<List<OnePasswordAccount>>("/onepassword") }
        loaded = true
    }
    val pending = if (loaded) "Indisponible" else "…"
    val summary = ConnectionSummary(state.agents.map { it.provider.ifBlank { "codex" } }.toSet(), accounts)
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).testTag("atelier")
            .padding(horizontal = 16.dp).padding(top = 12.dp, bottom = 24.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        ScreenTitle("Atelier", "Ce que vos agents peuvent utiliser.", Modifier.padding(start = 4.dp, bottom = 8.dp)) {
            RoundAction("Paramètres", LeoIcons.Gear) { navigate("settings") }
        }
        summary.missing.forEach { provider ->
            Surface(
                onClick = { navigate("connections") },
                shape = RoundedCornerShape(22.dp),
                color = signal.attentionSoft,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Row(Modifier.padding(start = 14.dp, end = 10.dp, top = 12.dp, bottom = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                    ProviderBrand(provider, 38.dp)
                    Spacer(Modifier.width(12.dp))
                    Column(Modifier.weight(1f)) {
                        Text("Compte ${providerLabel(provider)} requis", style = MaterialTheme.typography.titleSmall)
                        Text(
                            "Les agents ${providerLabel(provider)} ne peuvent pas démarrer.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    SignalButton("Connexions", height = 36.dp) { navigate("connections") }
                }
            }
        }
        Row(Modifier.height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Tile("Agents", state.agents.size, Modifier.weight(1f), { navigate("agents") }) {
                Row(horizontalArrangement = Arrangement.spacedBy((-8).dp)) {
                    state.agents.take(5).forEach {
                        Box(Modifier.clip(RoundedCornerShape(11.dp)).background(MaterialTheme.colorScheme.surface).padding(2.dp)) {
                            AgentAvatar(it.name, it.id, 30.dp)
                        }
                    }
                }
            }
            Tile("Projets", state.projects.size, Modifier.weight(1f), { navigate("projects") }) {
                Row(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
                    state.projects.take(6).forEach {
                        Box(Modifier.size(12.dp, 30.dp).clip(RoundedCornerShape(5.dp)).background(identityColor(it.id)))
                    }
                    if (state.projects.isEmpty()) TileIcon(LeoIcons.Folder)
                }
            }
        }
        Row(Modifier.height(IntrinsicSize.Min), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            val skills = state.skills.filter { it.valid }
            Tile("Skills", state.skills.size, Modifier.weight(1f), { navigate("skills") }) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    TileIcon(LeoIcons.Spark, MaterialTheme.colorScheme.primary)
                    if (skills.isNotEmpty()) {
                        Spacer(Modifier.width(6.dp))
                        Text(
                            skills.take(3).joinToString(", ") { it.name },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }
            val enabled = state.mcps.count { it.enabled }
            Tile("Serveurs MCP", state.mcps.size, Modifier.weight(1f), { navigate("mcps") }) {
                Row(
                    Modifier.semantics { contentDescription = "$enabled actifs sur ${state.mcps.size}" },
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    if (state.mcps.isEmpty()) TileIcon(LeoIcons.Plug)
                    state.mcps.take(6).forEach { mcp ->
                        Box(
                            Modifier.size(10.dp)
                                .then(
                                    if (mcp.enabled && mcp.state != "error") Modifier.background(signal.success, CircleShape)
                                    else if (mcp.enabled) Modifier.background(signal.attention, CircleShape)
                                    else Modifier.border(1.5.dp, MaterialTheme.colorScheme.onSurfaceVariant, CircleShape)
                                )
                        )
                    }
                    if (state.mcps.isNotEmpty())
                        Text(
                            if (enabled == 1) "1 actif" else "$enabled actifs",
                            Modifier.padding(start = 2.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
            }
        }
        SignalCard(Modifier.fillMaxWidth(), onClick = { navigate("connections") }) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Eyebrow("Connexions", Modifier.weight(1f))
                Icon(LeoIcons.Right, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Spacer(Modifier.height(12.dp))
            codingAgents.forEach { (provider, _) ->
                val ready = summary.ready(provider).size
                val total = summary.total(provider)
                val remaining = summary.remaining(provider)
                UsageLine(
                    provider,
                    providerLabel(provider),
                    when {
                        accounts == null -> pending
                        total == 0 -> "Aucun compte"
                        ready == 0 -> "Aucun compte disponible"
                        else -> "$ready/$total prêt${if (ready > 1) "s" else ""}" + (remaining?.let { " · ${it.toInt()} % restant" } ?: "")
                    },
                    remaining?.let { (100 - it) / 100 },
                    if (provider in summary.missing || (remaining ?: 100.0) < LowPercent) signal.attention else signal.success,
                )
                Spacer(Modifier.height(12.dp))
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(LeoIcons.Key, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
                Spacer(Modifier.width(10.dp))
                Text("1Password", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                Text(
                    when (val accounts = onePassword) {
                        null -> pending
                        else ->
                            if (accounts.isEmpty()) "Aucun compte"
                            else "${accounts.size} compte${if (accounts.size > 1) "s" else ""} · " +
                                "${accounts.flatMap { it.agentIds }.distinct().size} agent(s)"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(horizontal = 16.dp, vertical = 4.dp)) {
            LinkRow(LeoIcons.Gear, "Nodes et ressources") { navigate("nodes") }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            LinkRow(LeoIcons.Log, "Journal des exécutions") { navigate("runs") }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            LinkRow(LeoIcons.Shield, "Autoriser un assistant") { navigate("authorize") }
            HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
            LinkRow(LeoIcons.Gear, "Paramètres et accès") { navigate("settings") }
        }
        androidx.compose.material3.TextButton(
            onClick = {
                vm.clearMessage()
                signingOut = true
            },
            enabled = !state.busy,
            modifier = Modifier.padding(start = 4.dp),
        ) {
            Text("Se déconnecter")
        }
    }
    if (signingOut)
        Confirm(
            "Se déconnecter ?",
            "Les tâches continueront sur le serveur.",
            state.busy,
            state.error,
            { signingOut = false },
        ) {
            vm.perform { logout() }
        }
}

@Composable
private fun TileIcon(icon: ImageVector, tint: Color = MaterialTheme.colorScheme.onSurfaceVariant) =
    Icon(icon, null, Modifier.size(24.dp), tint = tint)

@Composable
private fun Tile(title: String, count: Int, modifier: Modifier, open: () -> Unit, art: @Composable () -> Unit) {
    SignalCard(modifier.fillMaxHeight(), onClick = open) {
        Box(Modifier.heightIn(min = 34.dp), contentAlignment = Alignment.CenterStart) { art() }
        Spacer(Modifier.weight(1f).heightIn(min = 14.dp))
        Row(verticalAlignment = Alignment.Bottom) {
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                count.toString(),
                style = MaterialTheme.typography.headlineSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun UsageLine(provider: String, name: String, label: String, used: Double?, tint: Color) {
    Row(verticalAlignment = Alignment.CenterVertically) {
        ProviderMark(provider, 20.dp)
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Row {
                Text(name, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall, maxLines = 1)
                Text(label, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1)
            }
            if (used != null) {
                Spacer(Modifier.height(6.dp))
                Box(Modifier.fillMaxWidth().height(6.dp).clip(CircleShape).background(MaterialTheme.colorScheme.surfaceVariant)) {
                    Box(
                        Modifier.fillMaxWidth(used.toFloat().coerceIn(0f, 1f)).fillMaxHeight().clip(CircleShape).background(tint)
                    )
                }
            }
        }
    }
}

@Composable
private fun LinkRow(icon: ImageVector, title: String, open: () -> Unit) {
    Surface(onClick = open, color = Color.Transparent, modifier = Modifier.fillMaxWidth()) {
        Row(Modifier.heightIn(min = 52.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(icon, null, Modifier.size(20.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.width(12.dp))
            Text(title, Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
            Icon(LeoIcons.Right, null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}
