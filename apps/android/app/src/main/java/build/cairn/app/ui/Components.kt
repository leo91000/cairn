@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package build.cairn.app.ui

import android.content.Intent
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.res.ResourcesCompat
import androidx.core.net.toUri
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import build.cairn.app.R
import build.cairn.app.data.*
import io.noties.markwon.Markwon
import io.noties.markwon.ext.strikethrough.StrikethroughPlugin
import io.noties.markwon.ext.tables.TablePlugin
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.withContext

fun date(value: Long?): String =
    value
        ?.takeIf { it > 0 }
        ?.let {
            DateTimeFormatter.ofPattern("dd MMM yyyy · HH:mm")
                .withZone(ZoneId.systemDefault())
                .format(Instant.ofEpochMilli(it))
        } ?: "—"

fun duration(run: Run): String =
    run.startedAt?.let {
        val seconds = ((run.finishedAt ?: System.currentTimeMillis()) - it).coerceAtLeast(0) / 1000
        "${seconds / 60} min ${seconds % 60} s"
    } ?: "—"

@Composable
inline fun <reified T : Any> rememberForm(initial: T): MutableState<T> =
    rememberSaveable(
        stateSaver =
            Saver(
                save = { wireJson.encodeToString(it) },
                restore = { wireJson.decodeFromString<T>(it) },
            )
    ) {
        mutableStateOf(initial)
    }

@Composable
fun Poll(key: Any?, interval: Long = 3000, action: suspend () -> Unit) {
    val owner = LocalLifecycleOwner.current
    val latest by rememberUpdatedState(action)
    LaunchedEffect(owner, key) {
        owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                latest()
                delay(interval)
            }
        }
    }
}

@Composable
fun Page(content: @Composable ColumnScope.() -> Unit) {
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        content = content,
    )
}

@Composable
fun Heading(title: String, subtitle: String = "") {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(title, style = MaterialTheme.typography.headlineSmall)
        if (subtitle.isNotBlank())
            Text(
                subtitle,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodyMedium,
            )
    }
}

@Composable
fun Panel(content: @Composable ColumnScope.() -> Unit) {
    SignalCard(Modifier.fillMaxWidth(), padding = PaddingValues(18.dp)) {
        Column(verticalArrangement = Arrangement.spacedBy(12.dp), content = content)
    }
}

@Composable
fun Empty(
    title: String = "Rien pour le moment",
    text: String = "Les éléments de votre espace apparaîtront ici.",
) {
    Panel {
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
fun ErrorNotice(message: String, dismiss: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.errorContainer, shape = MaterialTheme.shapes.medium) {
        Row(
            Modifier.fillMaxWidth().padding(start = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                message,
                Modifier.weight(1f).padding(vertical = 8.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onErrorContainer,
            )
            IconButton(onClick = dismiss) {
                Icon(
                    CairnIcons.Close,
                    "Masquer l’erreur",
                    Modifier.size(18.dp),
                    tint = MaterialTheme.colorScheme.onErrorContainer,
                )
            }
        }
    }
}

/**
 * A quiet line while the workspace cannot be reached. It is not an error to dismiss: it leaves on
 * its own once the connection returns.
 */
@Composable
fun ConnectionLostNotice(visible: Boolean) {
    androidx.compose.animation.AnimatedVisibility(
        visible,
        enter = androidx.compose.animation.expandVertically() + androidx.compose.animation.fadeIn(),
        exit = androidx.compose.animation.shrinkVertically() + androidx.compose.animation.fadeOut(),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(vertical = 4.dp).semantics {
                liveRegion = LiveRegionMode.Polite
            },
            horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(
                Modifier.size(10.dp),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                strokeWidth = 1.5.dp,
            )
            Text(
                "Connexion perdue · reconnexion…",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** Inline feedback for windows and pages that cannot show the workspace snackbar. */
internal data class ConnectionFeedback(val lost: Boolean = false, val notice: String? = null)

internal val LocalConnectionFeedback = compositionLocalOf { ConnectionFeedback() }

@Composable
internal fun ConnectionFeedbackNotice() {
    val feedback = LocalConnectionFeedback.current
    ConnectionLostNotice(feedback.lost)
    feedback.notice?.let {
        Text(
            it,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

internal fun statusLabel(value: String): String =
    when (value) {
        "suspended-exclusive" -> "Suspendue (exclusivité)"
        "running" -> "En cours"
        "queued" -> "En attente"
        "succeeded" -> "Terminée"
        "failed" -> "Échec"
        "cancelled" -> "Annulée"
        "interrupted" -> "Interrompue"
        else -> value
    }

internal fun runPresentationStatus(run: Run): String =
    if (run.active && run.suspendedByExclusive != null) "suspended-exclusive" else run.status

@Composable
fun Status(value: String) {
    val label = statusLabel(value)
    val (container, content) =
        when (value) {
            "failed",
            "interrupted" -> signal.attentionSoft to signal.attention
            "suspended-exclusive" -> signal.warning.copy(alpha = 0.14f) to signal.warning
            "running",
            "queued" ->
                MaterialTheme.colorScheme.primaryContainer to
                    MaterialTheme.colorScheme.onPrimaryContainer
            "succeeded" -> signal.success.copy(alpha = 0.14f) to signal.success
            else ->
                MaterialTheme.colorScheme.surfaceVariant to
                    MaterialTheme.colorScheme.onSurfaceVariant
        }
    Surface(color = container, shape = androidx.compose.foundation.shape.CircleShape) {
        Text(
            label,
            Modifier.padding(horizontal = 10.dp, vertical = 4.dp),
            style = MaterialTheme.typography.labelMedium,
            fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
            color = content,
        )
    }
}

@Composable
fun Field(
    label: String,
    value: String,
    onChange: (String) -> Unit,
    lines: Int = 1,
    enabled: Boolean = true,
    keyboardOptions: KeyboardOptions = InputKeyboards.Sentences,
) {
    OutlinedTextField(
        value,
        onChange,
        Modifier.fillMaxWidth(),
        label = { Text(label) },
        keyboardOptions = keyboardOptions,
        minLines = lines,
        maxLines = if (lines == 1) 1 else 20,
        singleLine = lines == 1,
        enabled = enabled,
    )
}

@Composable
fun Choice(
    label: String,
    selected: String,
    choices: List<Pair<String, String>>,
    onChange: (String) -> Unit,
) = Choice(label, selected, choices, enabled = true, onChange = onChange)

@Composable
fun Choice(
    label: String,
    selected: String,
    choices: List<Pair<String, String>>,
    enabled: Boolean,
    onChange: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Column {
        Text(label, style = MaterialTheme.typography.labelLarge)
        OutlinedButton(
            onClick = { expanded = true },
            enabled = enabled,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(choices.find { it.first == selected }?.second ?: "Choisir…")
        }
        DropdownMenu(expanded, { expanded = false }) {
            choices.forEach { (key, name) ->
                DropdownMenuItem(
                    text = { Text(name) },
                    enabled = enabled,
                    onClick = {
                        expanded = false
                        onChange(key)
                    },
                )
            }
        }
    }
}

@Composable
fun Toggle(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        Modifier.fillMaxWidth()
            .heightIn(min = 48.dp)
            .toggleable(value = checked, role = Role.Switch, onValueChange = onChange),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text(label, Modifier.weight(1f))
        Switch(checked, null)
    }
}

@Composable
fun Editor(
    title: String,
    busy: Boolean,
    error: String?,
    close: () -> Unit,
    save: () -> Unit,
    valid: Boolean = true,
    content: @Composable ColumnScope.() -> Unit,
) {
    val scroll = rememberScrollState()
    var revealFeedback by remember { mutableStateOf(false) }
    // Save feedback sits above the form: bring it back into view after a failed save.
    LaunchedEffect(busy, error) {
        if (!revealFeedback || busy) return@LaunchedEffect
        revealFeedback = false
        if (error != null) scroll.animateScrollTo(0)
    }
    Dialog(
        onDismissRequest = { if (!busy) close() },
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Scaffold(
            modifier = Modifier.fillMaxSize().imePadding(),
            topBar = {
                Column {
                    TopAppBar(
                        title = { Text(title) },
                        navigationIcon = {
                            IconButton(onClick = close, enabled = !busy) {
                                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Fermer")
                            }
                        },
                        actions = {
                            TextButton(
                                onClick = {
                                    revealFeedback = true
                                    save()
                                },
                                enabled = valid && !busy,
                            ) {
                                Text("Enregistrer")
                            }
                        },
                    )
                    Column(Modifier.padding(horizontal = 20.dp)) { ConnectionFeedbackNotice() }
                }
            },
        ) { padding ->
            Column(
                Modifier.padding(padding).fillMaxSize().verticalScroll(scroll).padding(20.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (error != null) Text(error, color = MaterialTheme.colorScheme.error)
                content()
            }
        }
    }
}

@Composable
fun Confirm(
    title: String,
    description: String,
    busy: Boolean,
    error: String?,
    dismiss: () -> Unit,
    action: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = { if (!busy) dismiss() },
        title = { Text(title) },
        text = {
            Column {
                Text(description)
                ConnectionFeedbackNotice()
                if (error != null) Text(error, color = MaterialTheme.colorScheme.error)
            }
        },
        confirmButton = { TextButton(onClick = action, enabled = !busy) { Text("Confirmer") } },
        dismissButton = { TextButton(onClick = dismiss, enabled = !busy) { Text("Annuler") } },
    )
}

/** Larger messages render off the main thread first, behind a reserved placeholder. */
private const val MARKDOWN_SYNC_RENDER = 8 * 1024

@Composable
fun Markdown(content: String) {
    val openArtifact by rememberUpdatedState(LocalArtifactLinks.current)
    val context = LocalContext.current
    val color = MaterialTheme.colorScheme.onSurface.toArgb()
    val linkColor = MaterialTheme.colorScheme.primary.toArgb()
    val markwon =
        remember(context) {
            Markwon.builder(context)
                .usePlugin(TablePlugin.create(context))
                .usePlugin(StrikethroughPlugin.create())
                .usePlugin(
                    object : io.noties.markwon.AbstractMarkwonPlugin() {
                        override fun configureConfiguration(
                            builder: io.noties.markwon.MarkwonConfiguration.Builder
                        ) {
                            builder.linkResolver { view, link ->
                                if (openArtifact(link)) return@linkResolver
                                val uri = link.toUri()
                                if (uri.scheme in setOf("https", "http") && uri.host != null) {
                                    runCatching {
                                        CustomTabsIntent.Builder()
                                            .build()
                                            .launchUrl(view.context, uri)
                                    }
                                        .onFailure {
                                            android.widget.Toast.makeText(
                                                    view.context,
                                                    "Aucune application disponible pour ouvrir ce lien.",
                                                    android.widget.Toast.LENGTH_LONG,
                                                )
                                                .show()
                                        }
                                }
                            }
                        }
                    }
                )
                .build()
        }
    val rendering = LocalMarkdownRendering.current
    val initial = remember(markwon, rendering) { MarkdownRenderPass(rendering) }
    DisposableEffect(markwon, rendering) {
        initial.begin()
        onDispose { initial.finish() }
    }
    val currentContent by rememberUpdatedState(content)
    val renderer = remember(markwon) { MarkdownBlocks(markwon) }
    // A row scrolled into view must be measured at its real height at once: an estimated
    // placeholder resizes after rendering and moves the text the reader is looking at.
    var rendered by
        remember(markwon) { mutableStateOf(content.takeIf { it.length <= MARKDOWN_SYNC_RENDER }) }
    var blocks by remember(markwon) { mutableStateOf(rendered?.let(renderer::render).orEmpty()) }
    LaunchedEffect(markwon, rendering) {
        // Keep displaying the last complete render while working. Conflation applies
        // to whole texts, after the stream accumulator has accepted every delta.
        snapshotFlow { currentContent }
            .conflate()
            .collect { text ->
                if (text != rendered) {
                    blocks = withContext(Dispatchers.Default) { renderer.render(text) }
                    rendered = text
                }
                withFrameNanos {}
                withFrameNanos {}
                initial.finish()
                rendering?.changed()
                delay(80)
            }
    }
    val copyText = remember { { currentContent } }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        if (blocks.isEmpty() && content.isNotBlank()) {
            // A zero-height lazy row can be skipped and disposed before its async
            // renderer finishes, making older messages impossible to scroll into.
            // Reserve bounded space until the first real measurement is available.
            val sample = content.take(2048)
            val lines = (sample.length / 48 + sample.count { it == '\n' } + 1).coerceIn(2, 32)
            val lineHeight =
                with(androidx.compose.ui.platform.LocalDensity.current) { 24.sp.toDp() }
            Spacer(Modifier.fillMaxWidth().height(lineHeight * lines))
        }
        blocks.forEachIndexed { index, block ->
            key(index) {
                if (block.tableColumns > 0) {
                    BoxWithConstraints(Modifier.fillMaxWidth()) {
                        val minimum =
                            with(androidx.compose.ui.platform.LocalDensity.current) {
                                (96 * block.tableColumns).sp.toDp()
                            }
                        val width = maxOf(maxWidth, minimum)
                        Box(
                            Modifier.fillMaxWidth()
                                .horizontalScroll(rememberScrollState())
                                .testTag("markdown-table")
                        ) {
                            MarkdownBlockView(
                                markwon,
                                block,
                                color,
                                linkColor,
                                copyText,
                                Modifier.width(width),
                            )
                        }
                    }
                } else {
                    MarkdownBlockView(markwon, block, color, linkColor, copyText)
                }
            }
        }
    }
}

@Composable
private fun MarkdownBlockView(
    markwon: Markwon,
    block: MarkdownBlock,
    color: Int,
    linkColor: Int,
    copyText: () -> String,
    modifier: Modifier = Modifier.fillMaxWidth(),
) {
    val textSizePx =
        with(androidx.compose.ui.platform.LocalDensity.current) {
            MaterialTheme.typography.bodyLarge.fontSize.toPx()
        }
    val lineHeightPx =
        with(androidx.compose.ui.platform.LocalDensity.current) {
            MaterialTheme.typography.bodyLarge.lineHeight.toPx()
        }
    AndroidView(
        factory = { context ->
            MarkdownTextView(context, copyText).apply {
                typeface = ResourcesCompat.getFont(context, R.font.cairn_body)
            }
        },
        modifier = modifier,
        update = {
            if (it.textSize != textSizePx)
                it.setTextSize(android.util.TypedValue.COMPLEX_UNIT_PX, textSizePx)
            val metrics = it.paint.fontMetrics
            val extra = (lineHeightPx - (metrics.descent - metrics.ascent)).coerceAtLeast(0f)
            if (it.lineSpacingExtra != extra || it.lineSpacingMultiplier != 1f)
                it.setLineSpacing(extra, 1f)
            it.setTextColor(color)
            it.setLinkTextColor(linkColor)
            it.bind(markwon, block)
        },
    )
}

@Composable
fun Code(value: String) {
    SelectionContainer {
        Text(value, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
fun ShareButton(text: String) {
    val context = LocalContext.current
    ActionIcon(
        "Partager",
        Icons.Default.Share,
        onClick = {
            context.startActivity(
                Intent.createChooser(
                    Intent(Intent.ACTION_SEND).apply {
                        type = "text/plain"
                        putExtra(Intent.EXTRA_TEXT, text)
                    },
                    "Partager",
                )
            )
        },
    )
}

@Composable
fun ExternalButton(label: String, url: String) {
    val context = LocalContext.current
    val uri = url.toUri()
    if (uri.scheme in setOf("https", "http") && uri.host != null) {
        OutlinedButton(
            onClick = { runCatching { CustomTabsIntent.Builder().build().launchUrl(context, uri) } }
        ) {
            Text(label)
        }
    }
}

@Composable
fun CopyButton(label: String, value: String, sensitive: Boolean = false) {
    val context = LocalContext.current
    var copied by remember(value) { mutableStateOf(false) }
    TextButton(
        onClick = {
            val clipboard = context.getSystemService(android.content.ClipboardManager::class.java)
            val clip = android.content.ClipData.newPlainText(label, value)
            if (sensitive)
                clip.description.extras =
                    android.os.PersistableBundle().apply {
                        putBoolean("android.content.extra.IS_SENSITIVE", true)
                    }
            clipboard.setPrimaryClip(clip)
            copied = true
        }
    ) {
        Text(if (copied) "Copié" else label)
    }
}
