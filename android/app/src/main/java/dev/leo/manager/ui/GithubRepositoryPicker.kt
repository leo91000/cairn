package dev.leo.manager.ui

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
import dev.leo.manager.data.*
import java.time.Duration
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

private enum class Visibility(val label: String) {
    All("Tous"),
    Private("Privés"),
    Public("Publics"),
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun GithubRepositoryPicker(
    api: LeoApi,
    selected: String,
    enabled: Boolean = true,
    select: (GithubRepository) -> Unit,
) {
    var repositories by remember { mutableStateOf(emptyList<GithubRepository>()) }
    var nextPage by remember { mutableStateOf<Int?>(1) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var query by rememberSaveable { mutableStateOf("") }
    var visibility by rememberSaveable { mutableStateOf(Visibility.All) }
    var browsing by rememberSaveable { mutableStateOf(true) }
    val scope = rememberCoroutineScope()
    val scroll = rememberScrollState()
    val needle = query.trim()
    val filtered = repositories.filter {
        (visibility == Visibility.All || it.isPrivate == (visibility == Visibility.Private)) &&
            "${it.fullName} ${it.description} ${it.language}".contains(needle, true)
    }
    // Searching and filtering keep fetching pages until enough matches are visible.
    val needsMore =
        nextPage != null &&
            error == null &&
            (needle.isNotEmpty() || visibility != Visibility.All) &&
            filtered.size < 8
    suspend fun load() {
        val page = nextPage ?: return
        if (loading) return
        loading = true
        error = null
        try {
            val result = api.get<GithubRepositoryPage>("/github/repositories?page=$page")
            repositories = (repositories + result.repositories).distinctBy { it.fullName }
            nextPage = result.nextPage
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "GitHub indisponible. Réessayez."
        } finally {
            loading = false
        }
    }
    LaunchedEffect(api) { load() }
    // Infinite scrolling: fetch the next page as the list approaches its end.
    val nearEnd = browsing && repositories.isNotEmpty() && scroll.maxValue - scroll.value < 240
    // Loads run in the picker scope: restarting this effect when `loading` changes must not cancel
    // them.
    LaunchedEffect(needsMore, nearEnd, loading) {
        if ((needsMore || nearEnd) && !loading && error == null) scope.launch { load() }
    }
    LaunchedEffect(needle, visibility) { scroll.scrollTo(0) }
    val chosen = repositories.firstOrNull { it.fullName == selected }

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
                                chosen.fullName,
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
                "Choisissez un dépôt de la connexion GitHub partagée. Il sera cloné à l’enregistrement.",
                Modifier.weight(1f),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            IconButton(
                onClick = {
                    repositories = emptyList()
                    nextPage = 1
                    scope.launch { load() }
                },
                enabled = enabled && !loading,
            ) {
                if (loading) CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                else Icon(Icons.Default.Refresh, "Recharger les dépôts")
            }
        }
        SearchField("Rechercher un dépôt, un propriétaire, un langage", query, { query = it })
        val privateCount = repositories.count { it.isPrivate }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Visibility.entries.forEach { option ->
                val count =
                    when (option) {
                        Visibility.All -> repositories.size
                        Visibility.Private -> privateCount
                        Visibility.Public -> repositories.size - privateCount
                    }
                FilterChip(
                    selected = visibility == option,
                    onClick = { visibility = option },
                    label = { Text("${option.label} · $count") },
                    leadingIcon =
                        if (visibility == option)
                            ({ Icon(Icons.Default.Check, null, Modifier.size(16.dp)) })
                        else null,
                    shape = CircleShape,
                )
            }
        }
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
                            "Vérifiez la connexion GitHub dans Connexions.",
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
                if (repositories.isEmpty() && loading) repeat(4) { RepositorySkeleton() }
                filtered.forEach { repo ->
                    RepositoryRow(repo, needle, selected == repo.fullName, enabled) {
                        select(repo)
                        browsing = false
                    }
                }
                if (repositories.isNotEmpty() && !loading && error == null && filtered.isEmpty()) {
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
                            "Aucun dépôt correspondant.",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (needle.isNotEmpty() || visibility != Visibility.All)
                            TextButton(
                                onClick = {
                                    query = ""
                                    visibility = Visibility.All
                                }
                            ) {
                                Text("Effacer les filtres")
                            }
                    }
                }
                if (!loading && error == null && repositories.isEmpty() && nextPage == null) {
                    Text(
                        "Aucun dépôt disponible avec cette connexion GitHub.",
                        Modifier.padding(24.dp),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (repositories.isNotEmpty() && loading)
                    LinearProgressIndicator(Modifier.fillMaxWidth().padding(8.dp))
                if (nextPage != null && repositories.isNotEmpty() && !loading) {
                    TextButton(
                        onClick = { scope.launch { load() } },
                        enabled = enabled && error == null,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text("Charger plus de dépôts")
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RepositoryRow(
    repo: GithubRepository,
    needle: String,
    selected: Boolean,
    enabled: Boolean,
    choose: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        onClick = choose,
        enabled = enabled && !repo.imported,
        modifier = Modifier.fillMaxWidth().alpha(if (repo.imported) .55f else 1f),
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
            OwnerAvatar(owner(repo), 38)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        highlight(repo.fullName, needle, owner(repo).length),
                        Modifier.weight(1f, false),
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (repo.imported) Tag("Déjà ajouté")
                    if (selected)
                        Icon(
                            Icons.Default.Check,
                            "Sélectionné",
                            Modifier.size(18.dp),
                            tint = colors.primary,
                        )
                }
                if (repo.description.isNotBlank()) {
                    Text(
                        highlight(repo.description, needle, -1),
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodySmall,
                        color = colors.onSurfaceVariant,
                    )
                }
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(10.dp),
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    Meta(if (repo.isPrivate) "🔒 Privé" else "Public")
                    if (repo.language.isNotBlank()) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Box(
                                Modifier.size(8.dp)
                                    .background(languageColor(repo.language), CircleShape)
                            )
                            Spacer(Modifier.width(4.dp))
                            Meta(repo.language)
                        }
                    }
                    if (repo.stars > 0) Meta("★ ${stars(repo.stars)}")
                    if (repo.defaultBranch.isNotBlank()) Meta("⎇ ${repo.defaultBranch}")
                    if (repo.fork) Meta("Fork")
                    if (repo.archived) Meta("Archivé", colors.tertiary)
                    updated(repo.pushedAt)?.let { Meta(it) }
                }
            }
        }
    }
}

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
private fun RepositorySkeleton() {
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

private fun owner(repo: GithubRepository) =
    repo.owner.ifBlank { repo.fullName.substringBefore('/') }

// Bolds the repository name after `owner/` and marks the search match.
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

private val languageColors =
    mapOf(
        "TypeScript" to 0xFF3178C6,
        "JavaScript" to 0xFFF1E05A,
        "Rust" to 0xFFDEA584,
        "Kotlin" to 0xFFA97BFF,
        "Python" to 0xFF3572A5,
        "Go" to 0xFF00ADD8,
        "Vue" to 0xFF41B883,
        "Java" to 0xFFB07219,
        "Swift" to 0xFFF05138,
        "Ruby" to 0xFF701516,
        "PHP" to 0xFF4F5D95,
        "Shell" to 0xFF89E051,
        "HTML" to 0xFFE34C26,
        "CSS" to 0xFF663399,
        "C#" to 0xFF178600,
        "C++" to 0xFFF34B7D,
        "C" to 0xFF555555,
        "Dart" to 0xFF00B4AB,
    )

private fun languageColor(language: String) =
    languageColors[language]?.let { Color(it) } ?: Color.hsl(hue(language), .5f, .55f)

private fun stars(value: Int) =
    if (value >= 1000)
        "%.${if (value >= 10000) 0 else 1}fk".format(java.util.Locale.ROOT, value / 1000.0)
    else "$value"

internal fun updated(pushedAt: String, now: Instant = Instant.now()): String? {
    val time = runCatching { Instant.parse(pushedAt) }.getOrNull() ?: return null
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
