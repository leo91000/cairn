@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import build.cairn.app.data.*

/** Lines shown per file before the diff is folded; the rest stays one tap away. */
private const val DIFF_PREVIEW_LINES = 120

/**
 * GitHub origin of an imported skill: check for a newer commit, review the diff, update or detach.
 */
@Composable
internal fun SkillSourceSheet(
    vm: CairnViewModel,
    model: SkillWorkbenchModel,
    draft: SkillDraft,
    owner: Boolean,
    close: () -> Unit,
) {
    val source = draft.base?.source ?: return
    val status = model.source
    var confirmUpdate by remember { mutableStateOf(false) }
    var confirmDetach by remember { mutableStateOf(false) }
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
    ) {
        Column(
            Modifier.testTag("skill-source-sheet")
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 20.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(
                    Modifier.size(40.dp).clip(RoundedCornerShape(12.dp)).background(signal.ink),
                    contentAlignment = Alignment.Center,
                ) {
                    Icon(CairnIcons.Branch, null, Modifier.size(22.dp), tint = signal.onInk)
                }
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text("Source GitHub", style = MaterialTheme.typography.titleLarge)
                    Text(
                        source.repository,
                        style = MaterialTheme.typography.bodyMedium,
                        fontFamily = FontFamily.Monospace,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                ActionIcon("Fermer la source", CairnIcons.Close, onClick = close)
            }
            SignalCard(
                Modifier.fillMaxWidth(),
                color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.5f),
                outlined = false,
            ) {
                SourceField("Dépôt", source.repository)
                SourceField("Dossier", source.path.ifBlank { "racine du dépôt" })
                SourceField("Référence", source.ref.ifBlank { "branche par défaut" })
                SourceField("Commit importé", source.commit.take(12))
                SourceField("Importé le", date(source.importedAt))
            }
            if (source.modified)
                Banner(
                    "Modifié dans Cairn",
                    "Une mise à jour remplacera les fichiers par ceux de GitHub ; vos modifications locales seront perdues.",
                    signal.warning,
                    signal.warningSoft,
                    Modifier.testTag("skill-source-modified"),
                    inset = 0.dp,
                )
            if (!owner) {
                Text(
                    "Seul le propriétaire peut rechercher des mises à jour.",
                    style = MaterialTheme.typography.bodySmall,
                )
            } else if (status == null) {
                SignalButton(
                    if (model.sourceLoading) "Vérification…" else "Rechercher des mises à jour",
                    Modifier.fillMaxWidth().testTag("skill-source-check"),
                    icon = CairnIcons.Retry,
                    enabled = !model.sourceLoading,
                    expand = true,
                ) {
                    model.checkSource(vm)
                }
                if (model.sourceLoading) LinearProgressIndicator(Modifier.fillMaxWidth())
            } else {
                LatestCommit(status)
                if (!status.current) {
                    Eyebrow(
                        "${plural(status.changes.size, "fichier modifié", "fichiers modifiés")}"
                    )
                    status.changes.forEach { SourceChangeCard(it) }
                }
            }
            if (owner) {
                if (status != null && !status.current)
                    SignalButton(
                        "Mettre à jour vers ${status.latest.commit.take(7)}",
                        Modifier.fillMaxWidth().testTag("skill-source-update"),
                        icon = CairnIcons.Download,
                        container = MaterialTheme.colorScheme.primary,
                        content = MaterialTheme.colorScheme.onPrimary,
                        enabled = !model.busy,
                        expand = true,
                    ) {
                        confirmUpdate = true
                    }
                TextButton(
                    onClick = { confirmDetach = true },
                    enabled = !model.busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Détacher de GitHub")
                }
            }
        }
    }
    if (confirmUpdate && status != null) {
        val blocked = draft.changes().isNotEmpty()
        AlertDialog(
            onDismissRequest = { confirmUpdate = false },
            title = { Text("Mettre à jour depuis GitHub ?") },
            text = {
                Text(
                    when {
                        blocked -> "Enregistrez ou annulez d’abord vos modifications en cours."
                        source.modified ->
                            "Les fichiers du skill seront remplacés par ceux du commit ${status.latest.commit.take(7)}. Les modifications faites dans Cairn seront perdues."
                        else ->
                            "Les fichiers du skill seront remplacés par ceux du commit ${status.latest.commit.take(7)}."
                    }
                )
            },
            confirmButton = {
                if (!blocked)
                    TextButton(
                        onClick = {
                            confirmUpdate = false
                            model.updateSource(vm, status.latest.commit, close)
                        },
                        modifier = Modifier.testTag("skill-source-update-confirm"),
                    ) {
                        Text("Mettre à jour")
                    }
            },
            dismissButton = {
                TextButton(onClick = { confirmUpdate = false }) {
                    Text(if (blocked) "Fermer" else "Annuler")
                }
            },
        )
    }
    if (confirmDetach)
        AlertDialog(
            onDismissRequest = { confirmDetach = false },
            title = { Text("Détacher de GitHub ?") },
            text = {
                Text(
                    "Les fichiers restent dans Cairn, mais le skill ne pourra plus être mis à jour depuis ${source.repository}."
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmDetach = false
                        model.detachSource(vm, close)
                    }
                ) {
                    Text("Détacher")
                }
            },
            dismissButton = { TextButton(onClick = { confirmDetach = false }) { Text("Annuler") } },
        )
}

@Composable
private fun SourceField(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Text(
            label,
            Modifier.width(128.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            Modifier.weight(1f),
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun LatestCommit(status: SkillSourceStatus) {
    val color = if (status.current) signal.success else MaterialTheme.colorScheme.primary
    Surface(
        Modifier.fillMaxWidth().testTag("skill-source-latest"),
        shape = RoundedCornerShape(18.dp),
        color = color.copy(alpha = 0.10f),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (status.current) CairnIcons.Check else CairnIcons.Download,
                    null,
                    Modifier.size(18.dp),
                    tint = color,
                )
                Spacer(Modifier.width(8.dp))
                Text(
                    if (status.current) "À jour" else "Nouvelle version disponible",
                    style = MaterialTheme.typography.titleSmall,
                    color = color,
                )
                Spacer(Modifier.weight(1f))
                Text(
                    status.latest.commit.take(7),
                    style = MaterialTheme.typography.labelMedium,
                    fontFamily = FontFamily.Monospace,
                )
            }
            if (status.latest.message.isNotBlank())
                Text(
                    status.latest.message.lineSequence().first(),
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            if (status.latest.date.isNotBlank())
                Text(
                    sourceDate(status.latest.date),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

internal fun sourceDate(value: String): String = runCatching {
    date(java.time.Instant.parse(value).toEpochMilli())
}
    .getOrDefault(value)

@Composable
private fun SourceChangeCard(change: SkillSourceChange) {
    var expanded by remember(change.path) { mutableStateOf(false) }
    val (label, color) =
        when (change.status) {
            "added" -> "ajouté" to signal.success
            "removed" -> "supprimé" to signal.attention
            else -> "modifié" to MaterialTheme.colorScheme.primary
        }
    val lines = change.lines.orEmpty()
    val added = lines.count { it.kind == "added" }
    val removed = lines.count { it.kind == "removed" }
    SignalCard(
        Modifier.fillMaxWidth().testTag("skill-source-change-${change.path}"),
        padding = PaddingValues(0.dp),
        outlined = true,
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                change.path,
                Modifier.weight(1f),
                style = MaterialTheme.typography.labelLarge,
                fontFamily = FontFamily.Monospace,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (added > 0)
                Text(
                    "+$added",
                    Modifier.padding(start = 6.dp),
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.Bold,
                    color = signal.success,
                )
            if (removed > 0)
                Text(
                    "−$removed",
                    Modifier.padding(start = 6.dp),
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.Bold,
                    color = signal.attention,
                )
            Spacer(Modifier.width(8.dp))
            SkillPill(label, color, color.copy(alpha = 0.12f))
        }
        when {
            change.binary -> DiffNote("Fichier binaire : le contenu n’est pas comparé.")
            change.lines == null -> DiffNote("Fichier trop long pour afficher les différences.")
            lines.isNotEmpty() -> {
                val shown = if (expanded) lines else lines.take(DIFF_PREVIEW_LINES)
                BoxWithConstraints(
                    Modifier.fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.35f))
                ) {
                    val viewport = maxWidth
                    // Every row takes the widest line's width so the colours form full bands.
                    Column(
                        Modifier.horizontalScroll(rememberScrollState())
                            .padding(vertical = 6.dp)
                            .width(IntrinsicSize.Max)
                            .widthIn(min = viewport)
                            .testTag("skill-diff")
                    ) {
                        shown.forEach { DiffLine(it) }
                    }
                }
                if (lines.size > shown.size)
                    Text(
                        "Afficher ${plural(lines.size - shown.size, "ligne")} de plus",
                        Modifier.fillMaxWidth().clickable { expanded = true }.padding(14.dp),
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.primary,
                    )
            }
        }
    }
}

@Composable
private fun DiffNote(text: String) {
    Text(
        text,
        Modifier.padding(start = 14.dp, end = 14.dp, bottom = 12.dp),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun DiffLine(line: SkillDiffLine) {
    if (line.kind == "gap") {
        // The server trims unchanged context; a gap marks the skipped lines.
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            HorizontalDivider(
                Modifier.width(18.dp),
                color = MaterialTheme.colorScheme.outlineVariant,
            )
            Text(
                line.text.ifBlank { "…" },
                Modifier.padding(horizontal = 8.dp),
                style = MaterialTheme.typography.labelSmall,
                fontStyle = FontStyle.Italic,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            HorizontalDivider(
                Modifier.width(18.dp),
                color = MaterialTheme.colorScheme.outlineVariant,
            )
        }
        return
    }
    val (marker, color, container) =
        when (line.kind) {
            "added" -> Triple("+", signal.success, signal.success.copy(alpha = 0.12f))
            "removed" -> Triple("−", signal.attention, signal.attention.copy(alpha = 0.12f))
            else -> Triple(" ", MaterialTheme.colorScheme.onSurfaceVariant, Color.Transparent)
        }
    Row(Modifier.fillMaxWidth().background(container).padding(end = 14.dp)) {
        Text(
            marker,
            Modifier.width(24.dp).padding(start = 10.dp),
            fontFamily = FontFamily.Monospace,
            fontSize = 12.sp,
            lineHeight = 18.sp,
            color = color,
            fontWeight = FontWeight.Bold,
        )
        Text(
            line.text,
            fontFamily = FontFamily.Monospace,
            fontSize = 12.sp,
            lineHeight = 18.sp,
            color =
                if (line.kind == "context") MaterialTheme.colorScheme.onSurfaceVariant
                else MaterialTheme.colorScheme.onSurface,
            softWrap = false,
        )
    }
}
