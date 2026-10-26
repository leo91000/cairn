package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import build.cairn.app.data.ContextUsage
import java.util.Locale
import kotlin.math.roundToInt
import kotlinx.serialization.json.JsonObject

internal data class ContextSummary(
    /** Short form for the header: `41 k / 258 k · 16 %`, or `indisponible`. */
    val short: String,
    /** The occupancy, or why it is unknown. */
    val label: String,
    /** Each known detail: cache, window, measurement. */
    val details: List<String>,
    /** Share of the window in use, only when both values are reported. */
    val percent: Int?,
)

private fun count(value: Long) =
    String.format(Locale.FRANCE, "%,d", value).replace('\u202f', ' ').replace('\u00a0', ' ')

private fun tokens(value: Long): String =
    when {
        value >= 1_000_000 ->
            if (value % 1_000_000 == 0L) "${value / 1_000_000} M"
            else String.format(Locale.FRANCE, "%.1f M", value / 1_000_000.0)
        value >= 1000 -> "${(value / 1000.0).roundToInt()} k"
        else -> value.toString()
    }

/**
 * How full the agent's context is. A missing window keeps the share unknown, and a missing report
 * stays unavailable: nothing is inferred from cumulative usage.
 */
internal fun contextSummary(usage: ContextUsage?): ContextSummary {
    if (usage == null || usage.contextTokens <= 0)
        return ContextSummary(
            "indisponible",
            "Utilisation du contexte indisponible",
            emptyList(),
            null,
        )
    val window = usage.contextWindow?.takeIf { it > 0 }
    val percent = window?.let { minOf(100, (usage.contextTokens * 100.0 / it).roundToInt()) }
    val estimate = if (usage.estimated) "≈ " else ""
    val details = buildList {
        add(
            if (usage.estimated) "Estimation de l’agent"
            else "Mesuré sur la dernière requête du modèle"
        )
        add(window?.let { "Fenêtre de ${count(it)} jetons" } ?: "Taille de fenêtre non communiquée")
        usage.cachedTokens
            ?.takeIf { it > 0 }
            ?.let { add("${count(it)} jetons lus depuis le cache") }
    }
    return ContextSummary(
        if (window != null)
            "$estimate${tokens(usage.contextTokens)} / ${tokens(window)} · $percent %"
        else "$estimate${tokens(usage.contextTokens)} jetons",
        if (window != null)
            "$estimate${count(usage.contextTokens)} sur ${count(window)} jetons ($percent %)"
        else "$estimate${count(usage.contextTokens)} jetons · fenêtre inconnue",
        details,
        percent,
    )
}

/**
 * The coding agent compacted its context; the history above stays as it was. [state] is `running`,
 * `done`, `failed`, or `stopped` when its execution ended before the coding agent confirmed it.
 */
internal data class ContextCompaction(
    val state: String,
    val trigger: String? = null,
    val preTokens: Long? = null,
    val postTokens: Long? = null,
)

/** A compaction item's latest state, keeping what earlier updates of the same item reported. */
internal fun contextCompaction(
    item: JsonObject,
    eventType: String,
    previous: ContextCompaction?,
): ContextCompaction {
    val status = item.string("status")
    val count = { key: String -> item.string(key).toLongOrNull()?.takeIf { it > 0 } }
    return ContextCompaction(
        state =
            when {
                status == "failed" -> "failed"
                status == "completed" || eventType == "item.completed" -> "done"
                else -> "running"
            },
        trigger =
            item.string("trigger").takeIf { it == "auto" || it == "manual" } ?: previous?.trigger,
        preTokens = count("pre_tokens") ?: previous?.preTokens,
        postTokens = count("post_tokens") ?: previous?.postTokens,
    )
}

/**
 * How an event that ends a turn or run ends a compaction its coding agent never confirmed. A coding
 * agent may fail or be stopped without closing the compaction it started.
 */
internal fun compactionEnd(type: String, text: String): String? =
    when {
        type == "turn.failed" ||
            type == "runner.failed" ||
            (type == "status" && text == "failed") -> "failed"
        type == "turn.started" ||
            type == "turn.completed" ||
            (type == "status" && text in setOf("succeeded", "cancelled", "interrupted")) ->
            "stopped"
        else -> null
    }

internal fun compactionText(compaction: ContextCompaction): Pair<String, String> {
    val title =
        when {
            compaction.state == "running" -> "Compaction du contexte…"
            compaction.state == "failed" -> "Échec de la compaction du contexte"
            compaction.state == "stopped" -> "Compaction du contexte interrompue"
            compaction.trigger == "auto" -> "Contexte compacté automatiquement"
            else -> "Contexte compacté"
        }
    val change =
        if (compaction.preTokens != null && compaction.postTokens != null)
            "${tokens(compaction.preTokens)} → ${tokens(compaction.postTokens)} jetons"
        else ""
    val kept = if (compaction.state == "done") "Les messages précédents restent visibles" else ""
    return title to listOf(change, kept).filter { it.isNotBlank() }.joinToString(" · ")
}

/** Where the coding agent compacted its context. Messages above it stay as they were. */
@Composable
internal fun ContextCompactionRow(compaction: ContextCompaction) {
    val (title, detail) = compactionText(compaction)
    val color =
        if (compaction.state == "failed") MaterialTheme.colorScheme.error
        else MaterialTheme.colorScheme.onSurfaceVariant
    Row(
        Modifier.fillMaxWidth()
            .padding(vertical = 12.dp)
            .testTag("context-compaction")
            .clearAndSetSemantics {
                role = Role.Image
                contentDescription =
                    listOf(title, detail).filter { it.isNotBlank() }.joinToString(". ")
            },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        HorizontalDivider(Modifier.weight(1f))
        Column(
            Modifier.widthIn(max = 280.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                Icon(CairnIcons.Layers, null, Modifier.size(14.dp), tint = color)
                Text(
                    title,
                    style = MaterialTheme.typography.labelMedium,
                    fontWeight = FontWeight.SemiBold,
                    color = color,
                    textAlign = TextAlign.Center,
                )
            }
            if (detail.isNotBlank())
                Text(
                    detail,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
        }
        HorizontalDivider(Modifier.weight(1f))
    }
}
