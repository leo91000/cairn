package build.cairn.app.ui

import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.*
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.ListSourceProjectsInput
import build.cairn.contracts.listSourceProjects
import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

/**
 * The projects a plugin's project source offers through one instance, e.g. the repositories of a
 * GitHub account. Pages come from the plugin; the search filters what is loaded and keeps loading
 * pages while too few match, as a plugin may ignore the query.
 */
@Composable
fun PluginProjectPicker(
    api: CairnApi,
    source: ProjectSourceChoice,
    instance: String,
    selected: String,
    enabled: Boolean = true,
    select: (PluginProject) -> Unit,
) {
    val key = "${source.key}/$instance"
    var projects by remember(key) { mutableStateOf(emptyList<PluginProject>()) }
    var nextPage by remember(key) { mutableStateOf<Int?>(1) }
    var loading by remember(key) { mutableStateOf(false) }
    var error by remember(key) { mutableStateOf<String?>(null) }
    var query by rememberSaveable { mutableStateOf("") }
    var browsing by rememberSaveable(key) { mutableStateOf(true) }
    val scope = rememberCoroutineScope()
    val scroll = rememberScrollState()
    val needle = query.trim()
    val filtered = projects.filter { it.matches(needle) }
    // Searching keeps fetching pages until enough matches are visible.
    val needsMore = nextPage != null && error == null && needle.isNotEmpty() && filtered.size < 8
    suspend fun load() {
        val page = nextPage ?: return
        if (loading) return
        loading = true
        error = null
        try {
            val result =
                api.listSourceProjects(
                    ListSourceProjectsInput(
                        pluginId = source.entry.id,
                        source = source.source.name,
                        instance = instance,
                        page = page,
                        query = "",
                    )
                )
            projects = (projects + result.items).distinctBy { it.id }
            nextPage = result.nextPage
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "${source.entry.name} indisponible. Réessayez."
        } finally {
            loading = false
        }
    }
    LaunchedEffect(api, key) { load() }
    // Infinite scrolling: fetch the next page as the list approaches its end.
    val nearEnd = browsing && projects.isNotEmpty() && scroll.maxValue - scroll.value < 240
    // Loads run in the picker scope: restarting this effect when `loading` changes must not cancel
    // them.
    LaunchedEffect(needsMore, nearEnd, loading, key) {
        if ((needsMore || nearEnd) && !loading && error == null) scope.launch { load() }
    }
    LaunchedEffect(needle) { scroll.scrollTo(0) }
    val chosen = projects.firstOrNull { it.id == selected }

    Column(
        Modifier.fillMaxWidth().animateContentSize(),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        if (chosen != null && !browsing) {
            OutlinedCard(
                Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(16.dp),
                border = BorderStroke(1.5.dp, MaterialTheme.colorScheme.primary),
                colors =
                    CardDefaults.outlinedCardColors(
                        containerColor =
                            MaterialTheme.colorScheme.primaryContainer.copy(alpha = .35f)
                    ),
            ) {
                Row(
                    Modifier.padding(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    OwnerAvatar(owner(chosen), 44)
                    Column(Modifier.weight(1f)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Icon(
                                Icons.Default.CheckCircle,
                                null,
                                Modifier.size(16.dp),
                                tint = MaterialTheme.colorScheme.primary,
                            )
                            Spacer(Modifier.width(6.dp))
                            Text(
                                chosen.name,
                                fontWeight = FontWeight.SemiBold,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                        Text(
                            "Branche ${chosen.defaultBranch.ifBlank { "par défaut" }} · clonée à l’enregistrement",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    TextButton(onClick = { browsing = true }, enabled = enabled) {
                        Icon(Icons.Default.Edit, null, Modifier.size(16.dp))
                        Spacer(Modifier.width(4.dp))
                        Text("Changer")
                    }
                }
            }
            return@Column
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                source.source.description.ifBlank { "Choisissez un projet." } +
                    " Il sera cloné à l’enregistrement.",
                Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            IconButton(
                onClick = {
                    projects = emptyList()
                    nextPage = 1
                    scope.launch { load() }
                },
                enabled = enabled && !loading,
            ) {
                if (loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                else Icon(Icons.Default.Refresh, "Recharger les projets")
            }
        }
        SearchField("Rechercher un projet", query, { query = it })
        error?.let {
            Surface(
                color = MaterialTheme.colorScheme.errorContainer,
                shape = RoundedCornerShape(12.dp),
            ) {
                Row(
                    Modifier.fillMaxWidth().padding(12.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Icon(
                        Icons.Default.Warning,
                        null,
                        tint = MaterialTheme.colorScheme.onErrorContainer,
                    )
                    Column(Modifier.weight(1f)) {
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.onErrorContainer,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        Text(
                            "Vérifiez ce ${source.entry.manifest?.noun()?.lowercase() ?: "compte"} dans Atelier → Plugins → ${source.entry.name}.",
                            color = MaterialTheme.colorScheme.onErrorContainer,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    FilledTonalButton(
                        onClick = { scope.launch { load() } },
                        enabled = enabled && !loading,
                    ) {
                        Text("Réessayer")
                    }
                }
            }
        }
        Surface(
            Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(16.dp),
            color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = .35f),
            border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        ) {
            Column(
                Modifier.heightIn(max = 380.dp).verticalScroll(scroll).padding(6.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                if (projects.isEmpty() && loading) repeat(4) { ProjectSkeleton() }
                filtered.forEach { project ->
                    ProjectRow(project, needle, selected == project.id, enabled) {
                        select(project)
                        browsing = false
                    }
                }
                if (projects.isNotEmpty() && !loading && error == null && filtered.isEmpty()) {
                    Column(
                        Modifier.fillMaxWidth().padding(24.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Icon(
                            Icons.Default.Search,
                            null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        Text(
                            "Aucun projet correspondant.",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (needle.isNotEmpty())
                            TextButton(onClick = { query = "" }) { Text("Effacer la recherche") }
                    }
                }
                if (!loading && error == null && projects.isEmpty() && nextPage == null) {
                    Text(
                        "Aucun projet disponible avec ce ${source.entry.manifest?.noun()?.lowercase() ?: "compte"}.",
                        Modifier.padding(24.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (projects.isNotEmpty() && loading)
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(8.dp))
                if (nextPage != null && projects.isNotEmpty() && !loading) {
                    TextButton(
                        onClick = { scope.launch { load() } },
                        enabled = enabled && error == null,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text("Charger plus de projets")
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun ProjectRow(
    project: PluginProject,
    needle: String,
    selected: Boolean,
    enabled: Boolean,
    choose: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        onClick = choose,
        enabled = enabled && !project.imported,
        modifier = Modifier.fillMaxWidth().alpha(if (project.imported) .55f else 1f),
        shape = RoundedCornerShape(12.dp),
        color = if (selected) colors.primaryContainer.copy(alpha = .5f) else colors.surface,
        border =
            BorderStroke(
                if (selected) 1.5.dp else 1.dp,
                if (selected) colors.primary else colors.outlineVariant.copy(alpha = .6f),
            ),
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OwnerAvatar(owner(project), 38)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        highlight(project.name, needle, project.name.lastIndexOf('/')),
                        Modifier.weight(1f, false),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (project.imported) Tag("Déjà ajouté")
                    if (selected)
                        Icon(
                            Icons.Default.Check,
                            "Sélectionné",
                            Modifier.size(18.dp),
                            tint = colors.primary,
                        )
                }
                if (project.description.isNotBlank()) {
                    Text(
                        highlight(project.description, needle, -1),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.onSurfaceVariant,
                    )
                }
                val updated = project.updatedAt?.let { updated(it) }
                if (
                    project.badges.isNotEmpty() ||
                        project.defaultBranch.isNotBlank() ||
                        updated != null
                )
                    FlowRow(
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp),
                    ) {
                        project.badges.forEach { Badge(it) }
                        if (project.defaultBranch.isNotBlank()) Meta("⎇ ${project.defaultBranch}")
                        updated?.let { Meta(it) }
                    }
            }
        }
    }
}

/** One of the plugin's short labels, such as a visibility or a language. */
@Composable
private fun Badge(text: String) =
    Text(
        text,
        Modifier.background(MaterialTheme.colorScheme.surfaceContainerHigh, CircleShape)
            .padding(horizontal = 7.dp, vertical = 1.dp),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        maxLines = 1,
    )

@Composable
private fun Meta(text: String, color: Color = MaterialTheme.colorScheme.onSurfaceVariant) =
    Text(text, style = MaterialTheme.typography.labelSmall, color = color)

@Composable
private fun Tag(text: String) =
    Surface(color = MaterialTheme.colorScheme.secondaryContainer, shape = CircleShape) {
        Text(
            text,
            Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
        )
    }

@Composable
private fun OwnerAvatar(owner: String, size: Int) =
    Box(
        Modifier.size(size.dp)
            .background(Color.hsl(hue(owner), .45f, .5f), RoundedCornerShape(10.dp)),
        contentAlignment = Alignment.Center,
    ) {
        Text(owner.take(1).uppercase(), color = Color.White, fontWeight = FontWeight.SemiBold)
    }

@Composable
private fun ProjectSkeleton() {
    val pulse by
        rememberInfiniteTransition(label = "skeleton")
            .animateFloat(
                .35f,
                .8f,
                infiniteRepeatable(tween(800), RepeatMode.Reverse),
                label = "pulse",
            )
    val tone = MaterialTheme.colorScheme.onSurface.copy(alpha = .1f)
    Row(
        Modifier.fillMaxWidth().alpha(pulse).padding(12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(Modifier.size(38.dp).background(tone, RoundedCornerShape(10.dp)))
        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Box(
                Modifier.fillMaxWidth(.45f).height(12.dp).background(tone, RoundedCornerShape(4.dp))
            )
            Box(Modifier.fillMaxWidth(.8f).height(10.dp).background(tone, RoundedCornerShape(4.dp)))
        }
    }
}

/** The part before the last `/` of a name such as `owner/name`, else the name itself. */
private fun owner(project: PluginProject) =
    project.name.substringBeforeLast('/').ifBlank { project.name }

// Bolds a name after its `owner/` prefix and marks the search match.
@Composable
private fun highlight(text: String, needle: String, ownerLength: Int): AnnotatedString {
    val mark =
        SpanStyle(
            background = MaterialTheme.colorScheme.tertiaryContainer,
            color = MaterialTheme.colorScheme.onTertiaryContainer,
        )
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    return buildAnnotatedString {
        append(text)
        if (ownerLength >= 0) {
            addStyle(SpanStyle(color = muted), 0, minOf(ownerLength + 1, text.length))
            addStyle(
                SpanStyle(fontWeight = FontWeight.SemiBold),
                minOf(ownerLength + 1, text.length),
                text.length,
            )
        }
        val at = if (needle.isEmpty()) -1 else text.indexOf(needle, ignoreCase = true)
        if (at >= 0) addStyle(mark, at, at + needle.length)
    }
}

private fun hue(value: String) =
    (value.fold(0) { hash, char -> hash * 31 + char.code }.toUInt() % 360u).toFloat()

internal fun updated(updatedAt: String, now: Instant = Instant.now()): String? {
    val time = runCatching { Instant.parse(updatedAt) }.getOrNull() ?: return null
    val minutes = Duration.between(time, now).toMinutes().coerceAtLeast(0)
    fun plural(n: Long, unit: String) =
        "Mis à jour il y a $n $unit${if (n > 1 && !unit.endsWith("s")) "s" else ""}"
    return when {
        minutes < 1 -> "Mis à jour à l’instant"
        minutes < 60 -> plural(minutes, "minute")
        minutes < 1440 -> plural(minutes / 60, "heure")
        minutes < 10080 -> plural(minutes / 1440, "jour")
        minutes < 43200 -> plural(minutes / 10080, "semaine")
        minutes < 525600 -> "Mis à jour il y a ${minutes / 43200} mois"
        else -> plural(minutes / 525600, "an")
    }
}
