@file:OptIn(ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import build.cairn.app.data.CairnViewModel
import build.cairn.app.data.Workspace

/**
 * Under a blocked mission's outcome: who it waits on, and what this account can do about it. The
 * item comes from the Fil, so both always agree on where the mission sits.
 */
@Composable
internal fun MissionOutcomeActions(
    vm: CairnViewModel,
    item: FeedItem,
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
) {
    val runId = item.runId ?: return
    val taskId = item.taskId ?: return
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Column(Modifier.padding(bottom = 12.dp).testTag("mission-outcome-actions")) {
        HorizontalDivider(Modifier.padding(top = 8.dp, bottom = 10.dp))
        when {
            item.setAside == SetAside.REQUESTER && item.requester != null -> {
                Text(
                    "En attente de « ${item.requester.title} », qui a lancé cette mission. " +
                        "Rien à faire de votre côté.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = muted,
                )
                TextButton(
                    onClick = {
                        item.requester.chatId?.let(openChat) ?: item.requester.runId?.let(openRun)
                    }
                ) {
                    Text("Ouvrir « ${item.requester.title} »")
                }
            }
            item.setAside == SetAside.EXTERNAL ->
                Text(
                    "En attente d’une vérification externe, comme la CI ou une revue. " +
                        "Rien à faire de votre côté.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = muted,
                )
            item.setAside == SetAside.DISMISSED || item.kind == FeedKind.SNOOZED_TASK -> {
                Text(
                    if (item.kind == FeedKind.SNOOZED_TASK)
                        "Rappel prévu ${shortStamp(item.wakeAt ?: 0)}."
                    else "Écartée : elle reste hors de Pour vous jusqu’au prochain résultat.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = muted,
                )
                OutlinedButton(
                    onClick = { vm.restoreOutcome(runId, taskId) },
                    Modifier.padding(top = 8.dp),
                ) {
                    Icon(CairnIcons.Undo, null, Modifier.size(16.dp))
                    Spacer(Modifier.width(8.dp))
                    Text("Remettre dans Pour vous")
                }
            }
            else ->
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    itemVerticalAlignment = Alignment.CenterVertically,
                ) {
                    if (item.dismissible)
                        OutlinedButton(onClick = { vm.setAside(runId, taskId, item.stamp) }) {
                            Icon(CairnIcons.Check, null, Modifier.size(16.dp))
                            Spacer(Modifier.width(8.dp))
                            Text("Écarter")
                        }
                    OutlinedButton(
                        onClick = { vm.setAside(runId, taskId, item.stamp, tomorrowMorning()) }
                    ) {
                        Icon(CairnIcons.Clock, null, Modifier.size(16.dp))
                        Spacer(Modifier.width(8.dp))
                        Text("Me le rappeler demain")
                    }
                }
        }
    }
}

/** Where the Fil holds a mission run's blocked outcome, while it is the mission's latest. */
@Composable
internal fun rememberOutcomeItem(vm: CairnViewModel, state: Workspace, runId: String): FeedItem? {
    val chats = rememberFilConversations(vm, state).state?.chats.orEmpty()
    val activity = rememberFilActivity(vm).orEmpty()
    val feed = buildFeed(chats, activity, state.tasks, vm.localReads)
    return (feed.forYou + feed.waiting + feed.recent).find { it.runId == runId }
}
