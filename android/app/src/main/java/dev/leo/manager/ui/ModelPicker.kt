@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package dev.leo.manager.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.animateContentSize
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.LayoutDirection
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import kotlin.math.roundToInt
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

internal fun effortLabel(value: String): String =
    when (value) {
        "none" -> "Aucun"
        "minimal" -> "Minimal"
        "low" -> "Faible"
        "medium" -> "Moyen"
        "high" -> "Élevé"
        "xhigh" -> "Très élevé"
        "max" -> "Maximal"
        "ultra" -> "Ultra"
        "" -> "Par défaut"
        else -> value
    }

internal fun providerLabel(value: String) = if (value == "claude") "Claude Code" else "Codex"

internal fun selectedModel(
    catalog: ModelCatalog,
    model: String,
    defaultModel: String,
): CodexModel? {
    val name = model.ifBlank { defaultModel }
    return if (name.isNotBlank()) catalog.models.find { it.model == name }
    else catalog.models.find { it.isDefault }
}

internal val ClaudeTint = Color(0xFFD97757)
private val ModelCardShape = RoundedCornerShape(18.dp)

// Simple Icons (CC0) brand marks, matching the web app's provider icons.
private fun brandMark(name: String, path: String) =
    ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
        .addPath(addPathNodes(path), fill = SolidColor(Color.Black))
        .build()

private val ClaudeMark by lazy {
    brandMark(
        "Claude",
        "m4.714 15.956l4.718-2.648l.079-.23l-.08-.128h-.23l-.79-.048l-2.695-.073l-2.337-.097l-2.265-.122l-.57-.121l-.535-.704l.055-.353l.48-.321l.685.06l1.518.104l2.277.157l1.651.098l2.447.255h.389l.054-.158l-.133-.097l-.103-.098l-2.356-1.596l-2.55-1.688l-1.336-.972l-.722-.491L2 6.223l-.158-1.008l.656-.722l.88.06l.224.061l.893.686l1.906 1.476l2.49 1.833l.364.304l.146-.104l.018-.072l-.164-.274l-1.354-2.446l-1.445-2.49l-.644-1.032l-.17-.619a3 3 0 0 1-.103-.729L6.287.133L6.7 0l.995.134l.42.364l.619 1.415L9.735 4.14l1.555 3.03l.455.898l.243.832l.09.255h.159V9.01l.127-1.706l.237-2.095l.23-2.695l.08-.76l.376-.91l.747-.492l.583.28l.48.685l-.067.444l-.286 1.851l-.558 2.903l-.365 1.942h.213l.243-.242l.983-1.306l1.652-2.064l.728-.82l.85-.904l.547-.431h1.032l.759 1.129l-.34 1.166l-1.063 1.347l-.88 1.142l-1.263 1.7l-.79 1.36l.074.11l.188-.02l2.853-.606l1.542-.28l1.84-.315l.832.388l.09.395l-.327.807l-1.967.486l-2.307.462l-3.436.813l-.043.03l.049.061l1.548.146l.662.036h1.62l3.018.225l.79.522l.473.638l-.08.485l-1.213.62l-1.64-.389l-3.825-.91l-1.31-.329h-.183v.11l1.093 1.068l2.003 1.81l2.508 2.33l.127.578l-.321.455l-.34-.049l-2.204-1.657l-.85-.747l-1.925-1.62h-.127v.17l.443.649l2.343 3.521l.122 1.08l-.17.353l-.607.213l-.668-.122l-1.372-1.924l-1.415-2.168l-1.141-1.943l-.14.08l-.674 7.254l-.316.37l-.728.28l-.607-.461l-.322-.747l.322-1.476l.388-1.924l.316-1.53l.285-1.9l.17-.632l-.012-.042l-.14.018l-1.432 1.967l-2.18 2.945l-1.724 1.845l-.413.164l-.716-.37l.066-.662l.401-.589l2.386-3.036l1.439-1.882l.929-1.086l-.006-.158h-.055L4.138 18.56l-1.13.146l-.485-.456l.06-.746l.231-.243l1.907-1.312Z",
    )
}
private val OpenAiMark by lazy {
    brandMark(
        "OpenAI",
        "M22.282 9.821a6 6 0 0 0-.516-4.91a6.05 6.05 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a6 6 0 0 0-3.998 2.9a6.05 6.05 0 0 0 .743 7.097a5.98 5.98 0 0 0 .51 4.911a6.05 6.05 0 0 0 6.515 2.9A6 6 0 0 0 13.26 24a6.06 6.06 0 0 0 5.772-4.206a6 6 0 0 0 3.997-2.9a6.06 6.06 0 0 0-.747-7.073M13.26 22.43a4.48 4.48 0 0 1-2.876-1.04l.141-.081l4.779-2.758a.8.8 0 0 0 .392-.681v-6.737l2.02 1.168a.07.07 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494M3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085l4.783 2.759a.77.77 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646M2.34 7.896a4.5 4.5 0 0 1 2.366-1.973V11.6a.77.77 0 0 0 .388.677l5.815 3.354l-2.02 1.168a.08.08 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.872zm16.597 3.855l-5.833-3.387L15.119 7.2a.08.08 0 0 1 .071 0l4.83 2.791a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.407-.667m2.01-3.023l-.141-.085l-4.774-2.782a.78.78 0 0 0-.785 0L9.409 9.23V6.897a.07.07 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.8.8 0 0 0-.393.681zm1.097-2.365l2.602-1.5l2.607 1.5v2.999l-2.597 1.5l-2.607-1.5Z",
    )
}

@Composable
internal fun ProviderMark(provider: String, size: Dp = 20.dp, tint: Color? = null) {
    val claude = provider == "claude"
    Icon(
        if (claude) ClaudeMark else OpenAiMark,
        null,
        Modifier.size(size),
        tint = tint ?: if (claude) ClaudeTint else MaterialTheme.colorScheme.onSurface,
    )
}

/**
 * One entry point for the coding agent, model and reasoning effort, shared by the composer and
 * agent settings. Empty values preserve server inheritance. When [changeProvider] is set, the sheet
 * also switches between Codex and Claude Code; the caller supplies that provider's catalog.
 */
@Composable
fun ModelPicker(
    catalog: ModelCatalog,
    model: String,
    reasoning: String,
    defaultModel: String = "",
    defaultReasoning: String = "",
    inherit: Boolean = false,
    enabled: Boolean = true,
    refresh: (suspend () -> Unit)? = null,
    modifier: Modifier = Modifier,
    provider: String = "codex",
    changeProvider: ((String) -> Unit)? = null,
    switching: Boolean = false,
    field: Boolean = false,
    change: (String, String) -> Unit,
) {
    var open by rememberSaveable { mutableStateOf(false) }
    var refreshing by remember { mutableStateOf(false) }
    var refreshError by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    suspend fun reload() {
        if (refreshing || refresh == null) return
        refreshing = true
        refreshError = ""
        try {
            refresh()
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            refreshError = "Catalogue temporairement indisponible. Réessayez."
        } finally {
            refreshing = false
        }
    }
    val selected = selectedModel(catalog, model, defaultModel)
    val inheritedEffort =
        if (inherit && model.isBlank())
            defaultReasoning.ifBlank { selected?.defaultReasoningEffort.orEmpty() }
        else selected?.defaultReasoningEffort.orEmpty()
    val effectiveEffort = reasoning.ifBlank { inheritedEffort }
    val modelLabel =
        selected?.displayName?.ifBlank { selected.model }
            ?: model.ifBlank { defaultModel }.ifBlank { "Modèle par défaut" }
    val summary =
        listOfNotNull(
                if (changeProvider != null || field) providerLabel(provider) else null,
                modelLabel,
                effectiveEffort
                    .takeIf { it.isNotBlank() }
                    ?.let { "effort ${effortLabel(it).lowercase()}" },
            )
            .joinToString(" · ")
    Box(modifier) {
        if (field)
            FieldTrigger(provider, modelLabel, effectiveEffort, enabled, summary) { open = true }
        else PillTrigger(provider, modelLabel, effectiveEffort, enabled, summary) { open = true }
    }
    if (open)
        ModalBottomSheet(
            onDismissRequest = { open = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            containerColor = MaterialTheme.colorScheme.surface,
        ) {
            if (refresh != null) Poll(provider, 300_000) { reload() }
            var query by rememberSaveable { mutableStateOf("") }
            // Claude lists its own "default" alias. When the default row already resolves to it,
            // show one row instead of two equivalent choices.
            val alias =
                catalog.models
                    .find { it.model == "default" }
                    ?.takeIf { !inherit || defaultModel.isBlank() }
            val visible =
                catalog.models.filter {
                    (!it.hidden || it.model == model) && (it != alias || model == it.model)
                }
            val models = visible.filter {
                it.displayName.contains(query, true) || it.model.contains(query, true)
            }
            val reasoningFor: @Composable () -> Unit = {
                ReasoningControl(selected, reasoning, inheritedEffort, enabled) {
                    change(model, it)
                }
            }
            Column(
                Modifier.fillMaxWidth()
                    .testTag("model-settings-sheet")
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 20.dp)
                    .padding(bottom = 28.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(
                            if (changeProvider != null) "Assistant et modèle" else "Modèle",
                            style = MaterialTheme.typography.titleLarge,
                        )
                        if (inherit)
                            Text(
                                "Ces réglages s’appliquent au prochain message.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                    }
                    if (refresh != null)
                        IconButton(
                            onClick = { scope.launch { reload() } },
                            enabled = !refreshing,
                            modifier =
                                Modifier.testTag("refresh-models").semantics {
                                    contentDescription =
                                        if (refreshing) "Actualisation des modèles…"
                                        else "Actualiser les modèles"
                                },
                        ) {
                            if (refreshing)
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            else Icon(Icons.Default.Refresh, null, Modifier.size(20.dp))
                        }
                    FilledTonalButton(onClick = { open = false }) { Text("Terminé") }
                }
                if (changeProvider != null) {
                    SectionLabel("Assistant de code")
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                        modifier = Modifier.height(IntrinsicSize.Min),
                    ) {
                        codingAgents.forEach { (value, vendor) ->
                            ProviderTile(
                                value,
                                vendor,
                                provider == value,
                                enabled,
                                Modifier.weight(1f).fillMaxHeight(),
                            ) {
                                if (provider != value) {
                                    query = ""
                                    changeProvider(value)
                                }
                            }
                        }
                    }
                    AnimatedVisibility(switching) {
                        Row(
                            Modifier.fillMaxWidth()
                                .background(
                                    MaterialTheme.colorScheme.primaryContainer,
                                    RoundedCornerShape(14.dp),
                                )
                                .padding(horizontal = 14.dp, vertical = 10.dp),
                            horizontalArrangement = Arrangement.spacedBy(10.dp),
                        ) {
                            Icon(
                                Icons.Default.Info,
                                null,
                                Modifier.size(18.dp),
                                tint = MaterialTheme.colorScheme.primary,
                            )
                            Text(
                                "Au prochain message, ${providerLabel(provider)} reprend le contexte du chat et les fichiers existants.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurface,
                            )
                        }
                    }
                    Spacer(Modifier.height(4.dp))
                }
                SectionLabel("Modèle")
                if (refreshError.isNotBlank())
                    Text(
                        refreshError,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                if (visible.size > 6) SearchField("Rechercher un modèle", query) { query = it }
                val fallback =
                    selectedModel(catalog, "", defaultModel)?.let {
                        it.displayName.ifBlank { it.model }
                    } ?: defaultModel
                ModelCard(
                    if (inherit) "Modèle de l’agent" else "Modèle par défaut",
                    if (inherit)
                        fallback.ifBlank { null }?.let { "$it · réglage de l’agent" }
                            ?: "Suivre le réglage de l’agent"
                    else
                        alias?.description?.ifBlank { null }
                            ?: fallback
                                .ifBlank { null }
                                ?.let { "$it · choisi par ${providerLabel(provider)}" }
                            ?: "Suivre le réglage par défaut de ${providerLabel(provider)}",
                    model.isBlank(),
                    enabled,
                    badge = if (alias != null && !inherit) "Recommandé" else "",
                    reasoning = reasoningFor,
                ) {
                    change("", "")
                }
                models.forEach { item ->
                    ModelCard(
                        item.displayName.ifBlank { item.model },
                        item.description,
                        item.model == model,
                        enabled,
                        badge = if (item.isDefault) "Recommandé" else "",
                        reasoning = reasoningFor,
                    ) {
                        if (item.model != model) change(item.model, "")
                    }
                }
                if (models.isEmpty() && query.isNotBlank())
                    Text(
                        "Aucun modèle trouvé.",
                        Modifier.fillMaxWidth()
                            .padding(vertical = 12.dp)
                            .wrapContentWidth(Alignment.CenterHorizontally),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                if (model.isNotBlank() && catalog.models.none { it.model == model })
                    ModelCard(
                        model,
                        "Modèle enregistré · absent du catalogue",
                        true,
                        enabled,
                        reasoning = reasoningFor,
                    ) {}
                if (catalog.models.isEmpty())
                    Field(
                        "Nom du modèle",
                        model,
                        { change(it, "") },
                        keyboardOptions = InputKeyboards.Literal,
                    )
                if (catalog.stale || catalog.error.isNotBlank())
                    Text(
                        catalog.error.ifBlank {
                            "Catalogue enregistré ; actualisation en attente."
                        },
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
        }
}

@Composable
private fun PillTrigger(
    provider: String,
    model: String,
    effort: String,
    enabled: Boolean,
    summary: String,
    open: () -> Unit,
) {
    Surface(
        onClick = open,
        enabled = enabled,
        shape = CircleShape,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier =
            Modifier.testTag("model-picker").semantics {
                contentDescription = "Choisir l’assistant, le modèle et le raisonnement : $summary"
            },
    ) {
        Row(
            Modifier.heightIn(min = 34.dp).padding(start = 8.dp, end = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            ProviderMark(provider, 18.dp)
            Text(
                model,
                Modifier.weight(1f, fill = false),
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (effort.isNotBlank()) {
                Box(
                    Modifier.size(3.dp)
                        .background(MaterialTheme.colorScheme.onSurfaceVariant, CircleShape)
                )
                Text(
                    effortLabel(effort),
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                )
            }
            Icon(
                Icons.Default.KeyboardArrowDown,
                null,
                Modifier.size(18.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun FieldTrigger(
    provider: String,
    model: String,
    effort: String,
    enabled: Boolean,
    summary: String,
    open: () -> Unit,
) {
    Surface(
        onClick = open,
        enabled = enabled,
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surface,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier =
            Modifier.fillMaxWidth().testTag("model-picker").semantics {
                contentDescription = "Choisir l’assistant, le modèle et le raisonnement : $summary"
            },
    ) {
        Row(
            Modifier.padding(horizontal = 14.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Box(
                Modifier.size(40.dp)
                    .background(
                        MaterialTheme.colorScheme.surfaceVariant,
                        RoundedCornerShape(12.dp),
                    ),
                contentAlignment = Alignment.Center,
            ) {
                ProviderMark(provider, 22.dp)
            }
            Column(Modifier.weight(1f)) {
                Text(
                    providerLabel(provider),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    model,
                    style = MaterialTheme.typography.titleSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (effort.isNotBlank())
                    Text(
                        "Raisonnement ${effortLabel(effort).lowercase()}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
            }
            Icon(
                Icons.Default.KeyboardArrowDown,
                null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun SectionLabel(text: String) {
    Text(
        text.uppercase(),
        style = MaterialTheme.typography.labelSmall,
        fontWeight = FontWeight.SemiBold,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 4.dp).semantics { heading() },
    )
}

@Composable
internal fun ProviderTile(
    provider: String,
    detail: String,
    selected: Boolean,
    enabled: Boolean,
    modifier: Modifier,
    choose: () -> Unit,
) {
    val accent = if (provider == "claude") ClaudeTint else MaterialTheme.colorScheme.primary
    val border by
        animateColorAsState(
            if (selected) accent else MaterialTheme.colorScheme.outlineVariant,
            label = "provider-border",
        )
    Surface(
        selected = selected,
        onClick = choose,
        enabled = enabled,
        shape = RoundedCornerShape(16.dp),
        color = if (selected) accent.copy(alpha = 0.1f) else MaterialTheme.colorScheme.surface,
        border = BorderStroke(if (selected) 1.5.dp else 1.dp, border),
        modifier = modifier,
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            ProviderMark(provider, 26.dp)
            Column(Modifier.weight(1f)) {
                Text(providerLabel(provider), style = MaterialTheme.typography.titleSmall)
                Text(
                    detail,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun ModelCard(
    title: String,
    description: String,
    selected: Boolean,
    enabled: Boolean,
    badge: String = "",
    reasoning: @Composable () -> Unit,
    choose: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        shape = ModelCardShape,
        color =
            if (selected) colors.primaryContainer else colors.surfaceVariant.copy(alpha = 0.55f),
        contentColor = colors.onSurface,
        border = if (selected) BorderStroke(1.5.dp, colors.primary) else null,
        modifier = Modifier.fillMaxWidth().animateContentSize(),
    ) {
        Column {
            Row(
                Modifier.fillMaxWidth()
                    .selectable(
                        selected = selected,
                        enabled = enabled,
                        role = Role.RadioButton,
                        onClick = choose,
                    )
                    .padding(horizontal = 16.dp, vertical = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                        verticalArrangement = Arrangement.Center,
                    ) {
                        Text(title, style = MaterialTheme.typography.titleSmall)
                        if (badge.isNotBlank())
                            Text(
                                badge,
                                Modifier.background(colors.primary.copy(alpha = 0.12f), CircleShape)
                                    .padding(horizontal = 8.dp, vertical = 1.dp),
                                style = MaterialTheme.typography.labelSmall,
                                color = colors.primary,
                            )
                    }
                    if (description.isNotBlank())
                        Text(
                            description,
                            style = MaterialTheme.typography.bodySmall,
                            color = colors.onSurfaceVariant,
                            maxLines = if (selected) 4 else 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                }
                if (selected)
                    Box(
                        Modifier.size(24.dp).background(colors.primary, CircleShape),
                        contentAlignment = Alignment.Center,
                    ) {
                        Icon(
                            Icons.Default.Check,
                            null,
                            Modifier.size(16.dp),
                            tint = colors.onPrimary,
                        )
                    }
                else
                    Box(
                        Modifier.size(24.dp)
                            .border(1.5.dp, colors.outline.copy(alpha = 0.5f), CircleShape)
                    )
            }
            // The effort belongs to the chosen model, so it opens inside that model's card.
            if (selected)
                Column(
                    Modifier.fillMaxWidth()
                        .padding(horizontal = 12.dp)
                        .padding(bottom = 12.dp)
                        .background(colors.surface, RoundedCornerShape(14.dp))
                        .padding(horizontal = 14.dp, vertical = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    reasoning()
                }
        }
    }
}

@Composable
internal fun ReasoningControl(
    model: CodexModel?,
    value: String,
    default: String,
    enabled: Boolean,
    change: (String) -> Unit,
) {
    val order = listOf("none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra")
    val efforts =
        model
            ?.supportedReasoningEfforts
            .orEmpty()
            .distinctBy { it.reasoningEffort }
            .sortedBy {
                order.indexOf(it.reasoningEffort).let { rank ->
                    if (rank < 0) Int.MAX_VALUE else rank
                }
            }
    val effective = value.ifBlank { default }
    val index = efforts.indexOfFirst { it.reasoningEffort == effective }
    val haptic = LocalHapticFeedback.current
    val rtl = LocalLayoutDirection.current == LayoutDirection.Rtl
    val colors = MaterialTheme.colorScheme
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            "Raisonnement",
            Modifier.weight(1f),
            style = MaterialTheme.typography.labelLarge,
            color = colors.onSurfaceVariant,
        )
        Text(
            effortLabel(effective),
            style = MaterialTheme.typography.titleMedium,
            color = colors.primary,
        )
    }
    if (efforts.size > 1) {
        val primary = colors.primary
        val track = colors.surfaceContainerHighest
        val idle = colors.onSurfaceVariant.copy(alpha = 0.45f)
        Slider(
            value = index.coerceAtLeast(0).toFloat(),
            onValueChange = { position ->
                val next = efforts[position.roundToInt().coerceIn(efforts.indices)].reasoningEffort
                if (next != effective || value.isBlank()) {
                    haptic.performHapticFeedback(HapticFeedbackType.TextHandleMove)
                    change(next)
                }
            },
            enabled = enabled,
            valueRange = 0f..efforts.lastIndex.toFloat(),
            steps = efforts.size - 2,
            modifier =
                Modifier.fillMaxWidth().height(52.dp).testTag("reasoning-slider").semantics {
                    contentDescription = "Effort de raisonnement"
                    stateDescription =
                        if (index < 0) "Choisir un niveau" else effortLabel(effective)
                },
            thumb = {
                Box(
                    Modifier.size(34.dp)
                        .shadow(3.dp, CircleShape)
                        .background(Color.White, CircleShape)
                )
            },
            track = { state ->
                Canvas(Modifier.fillMaxWidth().height(40.dp)) {
                    // Slider lays out the track between the centers of the 34 dp thumb.
                    // Extend its capsule beneath the thumb at both ends so dots and stops align.
                    val radius = size.height / 2
                    val inset = 20.dp.toPx()
                    val fraction = state.value / efforts.lastIndex
                    scale(scaleX = if (rtl) -1f else 1f, scaleY = 1f) {
                        drawRoundRect(
                            track,
                            topLeft = Offset(-inset, 0f),
                            size = Size(size.width + 2 * inset, size.height),
                            cornerRadius = CornerRadius(radius),
                        )
                        drawRoundRect(
                            primary,
                            topLeft = Offset(-inset, 0f),
                            size = Size(2 * inset + size.width * fraction, size.height),
                            cornerRadius = CornerRadius(radius),
                        )
                        efforts.indices.forEach { step ->
                            drawCircle(
                                if (step <= state.value) Color.White.copy(alpha = 0.4f) else idle,
                                3.dp.toPx(),
                                Offset(size.width * step / efforts.lastIndex, radius),
                            )
                        }
                    }
                }
            },
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                effortLabel(efforts.first().reasoningEffort),
                style = MaterialTheme.typography.labelSmall,
                color = colors.onSurfaceVariant,
            )
            Text(
                effortLabel(efforts.last().reasoningEffort),
                style = MaterialTheme.typography.labelSmall,
                color = colors.onSurfaceVariant,
            )
        }
    } else if (efforts.size == 1) {
        EffortChip(
            effortLabel(efforts.single().reasoningEffort),
            value == efforts.single().reasoningEffort,
            enabled,
        ) {
            change(efforts.single().reasoningEffort)
        }
    }
    if (index >= 0 && efforts[index].description.isNotBlank())
        Text(
            efforts[index].description,
            style = MaterialTheme.typography.bodySmall,
            color = colors.onSurfaceVariant,
        )
    if (model == null)
        Text(
            "Catalogue indisponible pour ce modèle. Le réglage enregistré est conservé.",
            style = MaterialTheme.typography.bodySmall,
        )
    else if (efforts.isEmpty())
        Text(
            "Ce modèle ne propose pas de réglage du raisonnement.",
            style = MaterialTheme.typography.bodySmall,
        )
    else if (index < 0 && value.isNotBlank())
        Text(
            "Ce niveau n’est pas proposé par le modèle. Choisissez un niveau disponible ou le réglage par défaut.",
            style = MaterialTheme.typography.bodySmall,
            color = colors.error,
        )
    EffortChip(
        "Par défaut${if (default.isNotBlank()) " · ${effortLabel(default)}" else ""}",
        value.isBlank(),
        enabled,
        Modifier.testTag("reasoning-default"),
    ) {
        change("")
    }
}

@Composable
private fun EffortChip(
    label: String,
    selected: Boolean,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    choose: () -> Unit,
) {
    FilterChip(
        selected = selected,
        onClick = choose,
        enabled = enabled,
        label = { Text(label) },
        leadingIcon =
            if (selected) ({ Icon(Icons.Default.Check, null, Modifier.size(16.dp)) }) else null,
        shape = CircleShape,
        modifier = modifier,
    )
}
