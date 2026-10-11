@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*

/** How the app currently reaches the selected installation. */
internal enum class ConnectionRoute(val label: String, val summary: String) {
    DIRECT("Direct", "En ligne · Direct"),
    RELAY("Relais", "En ligne · Relais"),
    OFFLINE("Hors ligne", "Hors ligne"),
}

internal fun connectionRoute(installation: Installation?, transportRoute: String) =
    when {
        installation?.online != true -> ConnectionRoute.OFFLINE
        transportRoute == "direct" -> ConnectionRoute.DIRECT
        else -> ConnectionRoute.RELAY
    }

/** Green when online and direct, amber through the relay, grey when offline. */
@Composable
internal fun ConnectionDot(route: ConnectionRoute, modifier: Modifier = Modifier) {
    val color =
        when (route) {
            ConnectionRoute.DIRECT -> signal.success
            ConnectionRoute.RELAY -> signal.warning
            ConnectionRoute.OFFLINE -> MaterialTheme.colorScheme.onSurfaceVariant
        }
    Box(
        modifier.size(8.dp).background(color, CircleShape).semantics {
            contentDescription = "Connexion : ${route.label}"
        }
    )
}

/**
 * Top-level header: the installation replaces the wordmark. Touching it opens the Connexion sheet;
 * search and the account stay on the right.
 */
@Composable
internal fun InstallationHeader(
    state: Workspace,
    openConnection: () -> Unit,
    modifier: Modifier = Modifier,
    search: (() -> Unit)? = null,
    account: (() -> Unit)? = null,
) {
    val installation = state.installation ?: return
    val route = connectionRoute(installation, state.transportRoute)
    Row(
        modifier.fillMaxWidth().heightIn(min = 56.dp).testTag("installation-header"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Surface(
            onClick = openConnection,
            modifier =
                Modifier.weight(1f).semantics {
                    contentDescription = "Choisir une installation : ${installation.name}"
                },
            color = Color.Transparent,
            shape = RoundedCornerShape(12.dp),
        ) {
            Row(Modifier.padding(vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
                CairnLogo()
                Spacer(Modifier.width(10.dp))
                Column(Modifier.weight(1f, fill = false)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            installation.name,
                            Modifier.weight(1f, fill = false),
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                        Icon(CairnIcons.Down, null, Modifier.padding(start = 2.dp).size(18.dp))
                    }
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ConnectionDot(route)
                        Spacer(Modifier.width(6.dp))
                        Text(
                            route.summary,
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                        )
                    }
                }
            }
        }
        if (search != null) RoundAction("Rechercher", CairnIcons.Search, onClick = search)
        if (account != null) {
            Spacer(Modifier.width(4.dp))
            AccountButton(state.session.account?.email.orEmpty(), account)
        }
    }
}

@Composable
private fun AccountButton(email: String, open: () -> Unit) {
    Surface(
        onClick = open,
        modifier = Modifier.size(48.dp).semantics { contentDescription = "Compte et paramètres" },
        color = Color.Transparent,
        shape = CircleShape,
    ) {
        Box(contentAlignment = Alignment.Center) {
            Box(
                Modifier.size(40.dp)
                    .background(MaterialTheme.colorScheme.primaryContainer, CircleShape),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    initial(email.substringBefore('@')),
                    style = MaterialTheme.typography.titleSmall,
                    fontWeight = FontWeight.Bold,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        }
    }
}

/** What a conversation adds to the Connexion sheet. */
internal data class ConversationConnection(
    val agent: String,
    val project: String?,
    val run: Run? = null,
)

/**
 * Everything the headers no longer show: installation (with a switcher when there are several),
 * role, route, the conversation's runner and synchronization, its agent and project, and the
 * installation settings for owners.
 */
@Composable
internal fun ConnectionSheet(
    vm: CairnViewModel,
    state: Workspace,
    close: () -> Unit,
    openSettings: (() -> Unit)? = null,
    conversation: ConversationConnection? = null,
) {
    val installation = state.installation ?: return
    val route = connectionRoute(installation, state.transportRoute)
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(Modifier.testTag("connection-sheet").padding(bottom = 16.dp)) {
            Row(
                Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Connexion", Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                ActionIcon("Fermer Connexion", Icons.Default.Close, onClick = close)
            }
            Column(Modifier.padding(horizontal = 20.dp)) {
                ConnectionRow("Installation") {
                    if (state.session.installations.size > 1) {
                        var choosing by remember { mutableStateOf(false) }
                        Box {
                            TextButton(
                                onClick = { choosing = true },
                                enabled = !state.busy,
                                modifier =
                                    Modifier.semantics {
                                        contentDescription = "Changer d’installation"
                                    },
                                contentPadding = PaddingValues(start = 8.dp, end = 0.dp),
                            ) {
                                Text(
                                    installation.name,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                    style = MaterialTheme.typography.bodyLarge,
                                    color = MaterialTheme.colorScheme.onSurface,
                                )
                                Icon(
                                    CairnIcons.Down,
                                    null,
                                    Modifier.padding(start = 2.dp).size(18.dp),
                                    tint = MaterialTheme.colorScheme.onSurface,
                                )
                            }
                            DropdownMenu(choosing, { choosing = false }) {
                                state.session.installations.forEach { choice ->
                                    DropdownMenuItem(
                                        text = {
                                            Column {
                                                Text(choice.name)
                                                Text(
                                                    "${choice.role.label} · " +
                                                        if (choice.online) "En ligne"
                                                        else "Hors ligne",
                                                    style = MaterialTheme.typography.labelSmall,
                                                    color =
                                                        MaterialTheme.colorScheme.onSurfaceVariant,
                                                )
                                            }
                                        },
                                        trailingIcon = {
                                            if (choice.id == installation.id)
                                                Icon(CairnIcons.Check, "Sélectionnée")
                                        },
                                        onClick = {
                                            choosing = false
                                            close()
                                            if (choice.id != installation.id)
                                                vm.perform { selectInstallation(choice.id) }
                                        },
                                    )
                                }
                            }
                        }
                    } else ConnectionValue(installation.name)
                }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                ConnectionRow("Rôle") { ConnectionValue(installation.role.label) }
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                ConnectionRow("Route") {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ConnectionDot(route)
                        Spacer(Modifier.width(8.dp))
                        ConnectionValue(route.label)
                    }
                }
                // Runner and synchronization come from the run's placement, owner only; the card
                // stays hidden while the master runner is the only choice, so its spacing only
                // applies when it shows.
                val run = conversation?.run
                if (state.isOwner && run != null)
                    Layout(content = { NodePlacement(vm, run) }) { measurables, constraints ->
                        val gap = 8.dp.roundToPx()
                        val placeables = measurables.map {
                            it.measure(constraints.copy(minHeight = 0))
                        }
                        val height = placeables.maxOfOrNull { it.height } ?: 0
                        val width = placeables.maxOfOrNull { it.width } ?: 0
                        if (height == 0) layout(width, 0) {}
                        else
                            layout(width, height + 2 * gap) {
                                placeables.forEach { it.place(0, gap) }
                            }
                    }
                if (conversation != null) {
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    ConnectionRow("Agent · projet") {
                        ConnectionValue(
                            listOf(
                                    conversation.agent,
                                    conversation.project ?: "Tous les projets autorisés",
                                )
                                .joinToString(" · ")
                        )
                    }
                }
                if (state.isOwner && openSettings != null) {
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    Surface(
                        onClick = {
                            close()
                            openSettings()
                        },
                        color = Color.Transparent,
                        modifier = Modifier.fillMaxWidth().semantics { role = Role.Button },
                    ) {
                        Row(
                            Modifier.heightIn(min = 52.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                "Réglages de l’installation",
                                Modifier.weight(1f),
                                style = MaterialTheme.typography.bodyLarge,
                            )
                            Icon(CairnIcons.Right, null, Modifier.size(18.dp))
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ConnectionRow(label: String, value: @Composable () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 52.dp).semantics(mergeDescendants = true) {},
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            Modifier.padding(end = 16.dp),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.weight(1f))
        value()
    }
}

@Composable
private fun ConnectionValue(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyLarge,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
    )
}
