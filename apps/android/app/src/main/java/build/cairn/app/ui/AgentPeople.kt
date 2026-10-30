package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.AgentAccountInput
import build.cairn.contracts.AgentInput
import build.cairn.contracts.grantAgentAccount
import build.cairn.contracts.listAgentAccounts
import build.cairn.contracts.revokeAgentAccount
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch

private const val MEMBERS_NOT_LOADED = "Les membres n’ont pas pu être chargés."

/**
 * Who may use one agent, as the owner manages it: members come from the Beacon, the installation
 * keeps which of them may use the agent. Each switch applies at once, outside the page's draft.
 */
internal class AgentPeople {
    var members by mutableStateOf<List<InstallationMember>>(emptyList())
    var allowed by mutableStateOf<List<String>>(emptyList())
    var loading by mutableStateOf(false)
    var loaded by mutableStateOf(false)
    var error by mutableStateOf<String?>(null)
    // An installation older than account access has no such list: every member uses every agent.
    var outdated by mutableStateOf(false)
    var saving by mutableStateOf(emptySet<String>())
    var failure by mutableStateOf<Pair<String, String>?>(null)

    val rows: List<AgentAccessRow>
        get() = agentAccessRows(members, allowed)

    suspend fun load(vm: CairnViewModel, agentId: String) {
        loading = true
        error = null
        try {
            coroutineScope {
                val listed = async { vm.installationMembers() }
                val accounts = async { vm.api.listAgentAccounts(AgentInput(agentId)) }
                members = listed.await()
                allowed = accounts.await().accounts.map { it.accountId }
            }
            outdated = false
            loaded = true
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            outdated = e is ApiException && e.status == 404
            if (!outdated) error = e.message ?: MEMBERS_NOT_LOADED
        } finally {
            loading = false
        }
    }

    suspend fun change(vm: CairnViewModel, agentId: String, accountId: String, allow: Boolean) {
        if (accountId in saving) return
        saving = saving + accountId
        failure = null
        try {
            val input = AgentAccountInput(accountId = accountId, agentId = agentId)
            val output =
                if (allow) vm.api.grantAgentAccount(input) else vm.api.revokeAgentAccount(input)
            allowed = withAccess(allowed, accountId, output.accounts.map { it.accountId })
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            failure = accountId to (e.message ?: "L’accès n’a pas pu être modifié.")
        } finally {
            saving = saving - accountId
        }
    }
}

@Composable
internal fun PeopleSection(
    vm: CairnViewModel,
    state: Workspace,
    people: AgentPeople,
    agentId: String,
    agentName: String,
    creating: Boolean,
    installation: Boolean,
    powerColors: Pair<Color, Color>,
    // The page's scope: a change still lands when the owner leaves this section meanwhile.
    scope: CoroutineScope,
    retry: () -> Unit,
) {
    val name = agentName.ifBlank { "cet agent" }
    Text(
        "Les personnes que vous autorisez ici peuvent démarrer des conversations avec $name, les poursuivre et les orienter, répondre à ses questions et planifier ses missions. Tout le monde dans l’installation voit toujours chaque conversation et peut mettre en pause ou arrêter le travail.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(14.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(
            "L’agent agit comme un rôle",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
        )
        Text(
            "Qui peut l’utiliser obtient tout ce qu’il peut faire : ses projets, secrets, outils et pouvoirs. Ne donnez l’accès qu’à des personnes à qui vous confieriez tout cela.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
        )
    }
    if (installation) {
        val (container, content) = powerColors
        Column(
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(container)
                .padding(14.dp)
                .testTag("agent-people-installation"),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                "⚠ Cet agent gère toute l’installation",
                style = MaterialTheme.typography.titleSmall,
                color = content,
            )
            Text(
                "Par lui, qui peut l’utiliser peut travailler avec les autres agents et les modifier, comme s’il pouvait utiliser tous les agents.",
                style = MaterialTheme.typography.bodyMedium,
                color = content,
            )
        }
    }
    if (creating) {
        Text(
            "Vous seul pouvez utiliser un nouvel agent. Créez-le d’abord, puis choisissez ici qui d’autre peut l’utiliser.",
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .padding(14.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }
    if (people.outdated) {
        Text(
            "Tous les membres peuvent utiliser $name sur cette version de l’installation. Mettez l’installation à jour pour choisir qui peut l’utiliser.",
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(MaterialTheme.colorScheme.surfaceVariant)
                .padding(14.dp)
                .testTag("agent-people-outdated"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }
    people.error?.let { error ->
        Column(
            Modifier.fillMaxWidth()
                .clip(RoundedCornerShape(14.dp))
                .background(MaterialTheme.colorScheme.errorContainer)
                .padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "Les membres n’ont pas pu être chargés : l’accès à cet agent ne peut pas être affiché ni modifié.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            // The cause, when it says more than the sentence above.
            if (error != MEMBERS_NOT_LOADED)
                Text(
                    error,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onErrorContainer.copy(alpha = 0.8f),
                )
            OutlinedButton(onClick = retry, enabled = !people.loading) {
                Text(if (people.loading) "Chargement…" else "Réessayer")
            }
        }
        return
    }

    val email = state.session.account?.email.orEmpty()
    Surface(
        Modifier.fillMaxWidth().testTag("agent-people"),
        shape = RoundedCornerShape(16.dp),
        border =
            androidx.compose.foundation.BorderStroke(
                1.dp,
                MaterialTheme.colorScheme.outlineVariant,
            ),
        color = MaterialTheme.colorScheme.surface,
    ) {
        Column {
            PersonRow(
                email.ifBlank { "Propriétaire" },
                state.session.account?.id ?: email,
                "Vous · propriétaire",
                email.ifBlank { "Propriétaire de l’installation" },
            ) {
                Text(
                    "🔒 Toujours",
                    Modifier.clip(CircleShape)
                        .background(MaterialTheme.colorScheme.surfaceVariant)
                        .padding(horizontal = 10.dp, vertical = 4.dp),
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.SemiBold,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
            if (people.loading && !people.loaded)
                Box(Modifier.fillMaxWidth().padding(20.dp), contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.5.dp)
                }
            people.rows.forEach { row ->
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                val saving = row.accountId in people.saving
                val label =
                    if (row.known) "Autoriser ${row.email} à utiliser $name"
                    else "Retirer l’accès qu’un ancien membre garde à $name"
                PersonRow(
                    if (row.known) row.email else "?",
                    row.accountId,
                    row.email,
                    when {
                        !row.known -> "L’accès prend fin à la prochaine vérification des membres"
                        row.allowed -> "Peut utiliser cet agent"
                        else -> "Pas d’accès"
                    },
                    detailColor =
                        if (row.known && row.allowed) signal.success
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier =
                        Modifier.toggleable(
                                value = row.allowed,
                                enabled = !saving && (row.known || row.allowed),
                                role = Role.Switch,
                            ) { allow ->
                                scope.launch { people.change(vm, agentId, row.accountId, allow) }
                            }
                            .semantics(mergeDescendants = true) { contentDescription = label }
                            .testTag("agent-person-${row.accountId}"),
                ) {
                    if (saving)
                        CircularProgressIndicator(
                            Modifier.size(18.dp).padding(end = 4.dp),
                            strokeWidth = 2.dp,
                        )
                    Switch(row.allowed, null, enabled = !saving && (row.known || row.allowed))
                }
                people.failure
                    ?.takeIf { it.first == row.accountId }
                    ?.let { (_, message) ->
                        Text(
                            message,
                            Modifier.padding(start = 68.dp, end = 16.dp, bottom = 10.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
            }
            // A change for a row that is no longer listed still reports its failure.
            people.failure
                ?.takeIf { failed -> people.rows.none { it.accountId == failed.first } }
                ?.let { (_, message) ->
                    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                    Text(
                        message,
                        Modifier.padding(16.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            if (people.loaded && people.rows.isEmpty()) {
                HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                Text(
                    "Aucun membre pour l’instant. Invitez des personnes depuis le partage de l’installation sur le web, puis choisissez ici qui peut utiliser $name.",
                    Modifier.padding(16.dp),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
    Text(
        "Les changements s’appliquent tout de suite ; les messages qu’un membre a déjà mis en file partent quand même, sauf s’ils en sont retirés. Un membre sans accès continue de lire les conversations ; ses missions planifiées sur cet agent sont suspendues jusqu’au retour de l’accès.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun PersonRow(
    avatarName: String,
    identity: String,
    title: String,
    detail: String,
    detailColor: Color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier: Modifier = Modifier,
    trailing: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier.fillMaxWidth().heightIn(min = 64.dp).padding(horizontal = 16.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Box(
            Modifier.size(38.dp).clip(CircleShape).background(identityColor(identity)),
            contentAlignment = Alignment.Center,
        ) {
            Text(
                initial(avatarName),
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.Bold,
                color = Color.White,
            )
        }
        Column(Modifier.weight(1f)) {
            Text(
                title,
                style = MaterialTheme.typography.bodyLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = detailColor,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        trailing()
    }
}
