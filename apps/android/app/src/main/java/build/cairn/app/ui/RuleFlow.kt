@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleProblem
import kotlinx.serialization.json.*

@Composable
internal fun RuleFlowTree(
    nodes: List<JsonElement>,
    container: String,
    catalog: HookRuleCatalog?,
    agentName: (String) -> String,
    selected: String? = null,
    editable: Boolean = false,
    select: (String) -> Unit = {},
    insert: (String, Int, String) -> Unit = { _, _, _ -> },
    remove: (String) -> Unit = {},
    move: (String, Int) -> Unit = { _, _ -> },
    depth: Int = 0,
    problems: List<HookRuleProblem> = emptyList(),
) {
    Column(Modifier.fillMaxWidth()) {
        if (editable) RuleInsertPoint(container, 0, depth, insert)
        nodes.forEachIndexed { index, raw ->
            val node = raw.jsonObject
            val path = "$container/$index"
            val kind =
                if (node.text("type") == "action") node.obj("action").text("type")
                else node.text("type")
            val nodeProblems = problemsUnder(problems, path)
            val tone =
                when (kind) {
                    "if" -> MaterialTheme.colorScheme.tertiary
                    "forEach" -> MaterialTheme.colorScheme.secondary
                    "cancelConversation" -> MaterialTheme.colorScheme.error
                    else -> MaterialTheme.colorScheme.primary
                }
            val icon =
                when (kind) {
                    "if" -> CairnIcons.Branch
                    "forEach" -> CairnIcons.Retry
                    "wait" -> CairnIcons.Hourglass
                    "storeValue" -> CairnIcons.Book
                    "callTool" -> CairnIcons.Plug
                    "answerQuestion" -> CairnIcons.SideQuestion
                    "cancelConversation" -> CairnIcons.Stop
                    else -> CairnIcons.Orbit
                }
            val summary =
                when (node.text("type")) {
                    "if" ->
                        "Si / sinon" to
                            describeCondition(node.obj("condition")).ifBlank {
                                "Choisir les conditions"
                            }
                    "forEach" ->
                        "Pour chaque ${node.text("itemName")}" to describeValue(node.obj("items"))
                    else -> describeAction(node.obj("action"), catalog, agentName)
                }
            Surface(
                shape = RoundedCornerShape(20.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
                border =
                    BorderStroke(
                        if (selected == path) 2.dp else 1.dp,
                        if (nodeProblems.isNotEmpty()) MaterialTheme.colorScheme.error
                        else if (selected == path) tone
                        else MaterialTheme.colorScheme.outlineVariant,
                    ),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Column {
                    Row(
                        Modifier.fillMaxWidth()
                            .then(if (editable) Modifier.clickable { select(path) } else Modifier)
                            .padding(14.dp),
                        verticalAlignment = Alignment.Top,
                    ) {
                        Surface(
                            shape = RoundedCornerShape(12.dp),
                            color = tone.copy(alpha = 0.12f),
                        ) {
                            Icon(icon, null, Modifier.padding(10.dp).size(19.dp), tint = tone)
                        }
                        Spacer(Modifier.width(10.dp))
                        Column(
                            Modifier.weight(1f),
                            verticalArrangement = Arrangement.spacedBy(3.dp),
                        ) {
                            Text(
                                node.text("label").ifBlank { summary.first },
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.SemiBold,
                            )
                            Text(
                                summary.second,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        if (editable)
                            Icon(
                                CairnIcons.Pencil,
                                "Modifier l’étape",
                                Modifier.size(16.dp),
                                tint = tone,
                            )
                    }
                    if (nodeProblems.isNotEmpty())
                        Text(
                            "${nodeProblems.size} points à corriger · ${nodeProblems.first().message}",
                            Modifier.padding(horizontal = 14.dp, vertical = 6.dp).clickable {
                                select(path)
                            },
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                            maxLines = 2,
                        )
                    if (editable)
                        Row(
                            Modifier.fillMaxWidth().padding(horizontal = 8.dp),
                            horizontalArrangement = Arrangement.End,
                        ) {
                            IconButton(
                                onClick = { move(path, -1) },
                                enabled = index > 0,
                                modifier = Modifier.size(36.dp),
                            ) {
                                Icon(CairnIcons.Up, "Monter", Modifier.size(17.dp))
                            }
                            IconButton(
                                onClick = { move(path, 1) },
                                enabled = index < nodes.lastIndex,
                                modifier = Modifier.size(36.dp),
                            ) {
                                Icon(CairnIcons.Down, "Descendre", Modifier.size(17.dp))
                            }
                            IconButton(
                                onClick = { remove(path) },
                                modifier = Modifier.size(36.dp),
                            ) {
                                Icon(
                                    CairnIcons.Trash,
                                    "Supprimer l’étape",
                                    Modifier.size(17.dp),
                                    tint = MaterialTheme.colorScheme.error,
                                )
                            }
                        }
                    if (node.text("type") == "if" || node.text("type") == "forEach") {
                        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        val lanes =
                            if (node.text("type") == "if")
                                listOf("then" to "ALORS", "otherwise" to "SINON")
                            else listOf("body" to "À CHAQUE ÉLÉMENT")
                        lanes.forEach { (branch, label) ->
                            Column(
                                Modifier.fillMaxWidth()
                                    .padding(
                                        start = if (depth < 3) 12.dp else 6.dp,
                                        end = 8.dp,
                                        top = 10.dp,
                                        bottom = 8.dp,
                                    ),
                                verticalArrangement = Arrangement.spacedBy(6.dp),
                            ) {
                                Text(
                                    label,
                                    color = tone,
                                    style = MaterialTheme.typography.labelSmall,
                                    fontWeight = FontWeight.Bold,
                                )
                                RuleFlowTree(
                                    node.list(branch),
                                    "$path/$branch",
                                    catalog,
                                    agentName,
                                    selected,
                                    editable,
                                    select,
                                    insert,
                                    remove,
                                    move,
                                    depth + 1,
                                    problems,
                                )
                            }
                        }
                    }
                }
            }
            if (editable) RuleInsertPoint(container, index + 1, depth, insert)
            else if (index < nodes.lastIndex)
                Box(
                    Modifier.padding(start = 32.dp)
                        .width(2.dp)
                        .height(16.dp)
                        .background(MaterialTheme.colorScheme.outlineVariant)
                )
        }
        if (!editable && nodes.isEmpty())
            Text(
                "Aucune étape dans ce chemin.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(8.dp),
            )
    }
}

@Composable
private fun RuleInsertPoint(
    container: String,
    index: Int,
    depth: Int,
    insert: (String, Int, String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box(Modifier.fillMaxWidth().padding(vertical = 8.dp), contentAlignment = Alignment.Center) {
        OutlinedButton(
            onClick = { expanded = true },
            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 2.dp),
            shape = CircleShape,
            modifier = Modifier.heightIn(min = 36.dp),
        ) {
            Icon(CairnIcons.Plus, null, Modifier.size(16.dp))
            Spacer(Modifier.width(4.dp))
            Text("Étape", style = MaterialTheme.typography.labelMedium)
        }
        DropdownMenu(expanded, { expanded = false }) {
            ruleStepChoices
                .filter {
                    !(it.kind == "wait" && container.contains("/body")) &&
                        !(depth >= 7 && it.kind in setOf("if", "forEach"))
                }
                .forEach { choice ->
                    DropdownMenuItem(
                        text = {
                            Column {
                                Text(choice.title)
                                Text(
                                    choice.description,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        },
                        onClick = {
                            expanded = false
                            insert(container, index, choice.kind)
                        },
                    )
                }
        }
    }
}
