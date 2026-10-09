package build.cairn.app.ui

import android.graphics.BitmapFactory
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import build.cairn.app.data.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*

private fun ActivityKind.icon(): ImageVector =
    when (this) {
        ActivityKind.COMMAND,
        ActivityKind.OUTPUT -> CairnIcons.Terminal
        ActivityKind.READ,
        ActivityKind.FILES -> CairnIcons.File
        ActivityKind.BROWSE -> CairnIcons.Layers
        ActivityKind.SEARCH -> Icons.Default.Search
        ActivityKind.PLAN -> Icons.Default.Check
        ActivityKind.THINKING -> Icons.Default.Star
        ActivityKind.TOOL -> CairnIcons.Tune
        ActivityKind.NOTICE -> Icons.Default.Info
    }

@Composable
internal fun ActivityCard(
    event: RunEvent,
    presentation: ActivityPresentation = presentActivity(event),
) {
    var expanded by rememberSaveable(event.id) { mutableStateOf(false) }
    val p = presentation
    val tint =
        if (p.failed) MaterialTheme.colorScheme.error
        else MaterialTheme.colorScheme.onSurfaceVariant
    Surface(
        color =
            if (p.kind == ActivityKind.NOTICE) Color.Transparent
            else MaterialTheme.colorScheme.surfaceContainerLow,
        shape = RoundedCornerShape(16.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column {
            Surface(onClick = { expanded = !expanded }, color = Color.Transparent) {
                Row(
                    Modifier.fillMaxWidth()
                        .padding(
                            horizontal = 12.dp,
                            vertical = if (p.kind == ActivityKind.NOTICE) 10.dp else 14.dp,
                        ),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Surface(
                        color =
                            if (p.failed) MaterialTheme.colorScheme.errorContainer
                            else MaterialTheme.colorScheme.surfaceVariant,
                        shape = RoundedCornerShape(12.dp),
                    ) {
                        Icon(
                            p.kind.icon(),
                            null,
                            Modifier.padding(10.dp)
                                .size(if (p.kind == ActivityKind.NOTICE) 16.dp else 20.dp),
                            tint = tint,
                        )
                    }
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        if (p.kind != ActivityKind.NOTICE)
                            Text(
                                p.kind.label.uppercase(),
                                style = MaterialTheme.typography.labelSmall,
                                color = tint,
                            )
                        Text(
                            p.title,
                            style =
                                if (p.kind == ActivityKind.NOTICE)
                                    MaterialTheme.typography.bodySmall
                                else MaterialTheme.typography.titleSmall,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                        if (p.subtitle.isNotBlank())
                            Text(
                                p.subtitle,
                                style = MaterialTheme.typography.bodySmall,
                                color = tint,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                        if (p.kind != ActivityKind.NOTICE || p.failed)
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                Outcome(p.state, p.failed)
                                p.exitCode?.let {
                                    Text(
                                        "Code $it",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = tint,
                                    )
                                }
                            }
                    }
                    Icon(
                        if (expanded) CairnIcons.Down else CairnIcons.Right,
                        if (expanded) "Réduire" else "Détails",
                        Modifier.size(18.dp),
                        tint = tint,
                    )
                }
            }
            if (expanded)
                Column(
                    Modifier.padding(horizontal = 14.dp).padding(bottom = 14.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    ActivityBody(p)
                    AdvancedSource(p.raw)
                }
        }
    }
}

@Composable
private fun Outcome(
    value: String,
    failed: Boolean =
        value.lowercase() in listOf("failed", "failure", "error", "échec", "timed_out"),
) {
    val label = friendlyStatus(value)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Icon(
            if (failed) Icons.Default.Warning
            else if (label in listOf("Réussi", "Terminé", "Fusionné")) Icons.Default.Check
            else Icons.Default.Info,
            null,
            Modifier.size(13.dp),
            tint =
                if (failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
        )
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color =
                if (failed) MaterialTheme.colorScheme.error
                else MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
internal fun ActivityBody(p: ActivityPresentation) {
    val item = p.item
    when (p.kind) {
        ActivityKind.COMMAND,
        ActivityKind.READ,
        ActivityKind.BROWSE -> {
            if (p.kind == ActivityKind.COMMAND) CodeBlock("Commande", p.command)
            else {
                p.paths.forEach { PathRow(it, "Lecture") }
                Disclosure("Commande exécutée") { CodeBlock("Terminal", p.command) }
            }
            item
                .string("cwd")
                .ifBlank { item.string("working_directory") }
                .takeIf { it.isNotEmpty() }
                ?.let { PathRow(it, "Dossier") }
            if (p.output.isBlank())
                Text(
                    if (p.state == "En cours") "En attente du résultat…"
                    else "Cette commande n’a produit aucune sortie.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            else if (
                p.kind == ActivityKind.READ && p.paths.singleOrNull()?.endsWith(".md", true) == true
            )
                Markdown(p.output)
            else if (
                p.kind == ActivityKind.READ &&
                    p.paths.singleOrNull()?.endsWith(".json", true) != true
            )
                CodeBlock("Contenu du fichier", p.output)
            else if (p.kind == ActivityKind.BROWSE) FileList(p.output)
            else if (
                p.command.startsWith("git diff") &&
                    p.output.lineSequence().any { it.startsWith("@@") }
            )
                DiffView(p.output)
            else if (resultParts(p.output).all { it is ResultPart.Text })
                CodeBlock("Sortie", p.output)
            else ResultContent(p.output)
        }
        ActivityKind.SEARCH -> {
            if (p.command.isNotBlank()) {
                Disclosure("Commande exécutée") { CodeBlock("Terminal", p.command) }
                if (p.output.isBlank()) Text(p.state, style = MaterialTheme.typography.bodySmall)
                else SearchMatches(p.output)
            } else {
                val action = item?.get("action").asObject()
                if (p.subtitle.isNotBlank())
                    Text(p.subtitle, style = MaterialTheme.typography.titleMedium)
                val results = item?.get("results") ?: item?.get("result") ?: action?.get("results")
                if (results != null) StructuredData(results)
                else
                    Text(
                        "Recherche effectuée. Les sources citées apparaissent dans la réponse.",
                        style = MaterialTheme.typography.bodySmall,
                    )
            }
        }
        ActivityKind.FILES -> {
            item?.get("changes").asArray().orEmpty().forEach { change ->
                val file = change.asObject()
                PathRow(
                    file.string("path"),
                    when (file.string("kind")) {
                        "add",
                        "create" -> "Ajout"
                        "delete" -> "Suppression"
                        else -> "Modification"
                    },
                )
                file
                    .string("diff")
                    .ifBlank { file.string("patch") }
                    .takeIf { it.isNotEmpty() }
                    ?.let { DiffView(it) }
            }
        }
        ActivityKind.PLAN -> PlanView(item?.get("items").asArray().orEmpty())
        ActivityKind.THINKING ->
            if (p.output.isNotBlank()) Markdown(p.output) else Text("Réflexion en cours…")
        ActivityKind.TOOL -> McpResult(item)
        ActivityKind.OUTPUT ->
            if (p.output.isNotBlank()) ResultContent(p.output)
            else
                (p.item ?: p.data)?.let {
                    StructuredData(
                        JsonObject(it.filterKeys { key -> key !in listOf("type", "id") })
                    )
                }
        ActivityKind.NOTICE -> {
            if (p.output.isNotBlank()) Text(p.output, style = MaterialTheme.typography.bodySmall)
            else if (p.subtitle.isBlank())
                Text(
                    if (p.failed) "Consultez les détails techniques pour diagnostiquer cette étape."
                    else "${p.title}.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            val usage = p.data?.get("usage")
            if (usage != null) StructuredData(usage)
        }
    }
}

@Composable
internal fun Disclosure(
    label: String,
    initiallyOpen: Boolean = false,
    content: @Composable () -> Unit,
) {
    var open by rememberSaveable { mutableStateOf(initiallyOpen) }
    Column {
        Surface(
            onClick = { open = !open },
            color = Color.Transparent,
            shape = MaterialTheme.shapes.small,
        ) {
            Row(
                Modifier.fillMaxWidth().heightIn(min = 48.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Icon(if (open) CairnIcons.Down else CairnIcons.Right, null, Modifier.size(16.dp))
                Text(label, style = MaterialTheme.typography.labelLarge)
            }
        }
        if (open) content()
    }
}

@Composable
internal fun AdvancedSource(source: String) {
    Disclosure("Détails techniques · JSON / source") { CodeBlock("Source enregistrée", source) }
}

@Composable
private fun CodeBlock(label: String, source: String) {
    var limit by rememberSaveable(source.length) { mutableIntStateOf(8000) }
    Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        shape = RoundedCornerShape(12.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                label,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Box(Modifier.horizontalScroll(rememberScrollState())) { Code(source.take(limit)) }
            if (source.length > limit)
                TextButton(onClick = { limit += 16000 }) { Text("Afficher la suite") }
        }
    }
}

@Composable
private fun PathRow(path: String, change: String = "") {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Icon(CairnIcons.File, null, Modifier.size(18.dp), tint = MaterialTheme.colorScheme.primary)
        Column(Modifier.weight(1f)) {
            Text(
                path,
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
            )
            if (change.isNotBlank())
                Text(
                    change,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
        }
    }
}

@Composable
private fun FileList(output: String) {
    val lines = output.lineSequence().filter { it.isNotBlank() }.toList()
    var limit by rememberSaveable { mutableIntStateOf(12) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        lines.take(limit).forEach { PathRow(it) }
        if (lines.size > limit)
            TextButton(onClick = { limit += 50 }) { Text("${lines.size-limit} autres lignes") }
    }
}

@Composable
private fun SearchMatches(output: String) {
    val lines = output.lineSequence().toList()
    var limit by rememberSaveable { mutableIntStateOf(12) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        lines.take(limit).forEach { line ->
            val match = Regex("^(.+?):(\\d+):(.*)$").matchEntire(line)
            if (match == null) Text(line, style = MaterialTheme.typography.bodySmall)
            else
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = MaterialTheme.shapes.small,
                ) {
                    Column(Modifier.fillMaxWidth().padding(10.dp)) {
                        Text(
                            "${match.groupValues[1]} · ligne ${match.groupValues[2]}",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary,
                        )
                        Text(
                            match.groupValues[3],
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                        )
                    }
                }
        }
        if (lines.size > limit)
            TextButton(onClick = { limit += 50 }) { Text("${lines.size-limit} autres lignes") }
    }
}

@Composable
private fun DiffView(source: String) {
    val lines = source.lines()
    val added = lines.count { it.startsWith('+') && !it.startsWith("+++") }
    val removed = lines.count { it.startsWith('-') && !it.startsWith("---") }
    var limit by rememberSaveable { mutableIntStateOf(80) }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            "+$added ajouts · −$removed suppressions",
            style = MaterialTheme.typography.labelMedium,
        )
        Surface(
            color = MaterialTheme.colorScheme.surfaceVariant,
            shape = RoundedCornerShape(12.dp),
        ) {
            Column(Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(10.dp)) {
                lines.take(limit).forEach { line ->
                    Text(
                        line,
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                        color =
                            when {
                                line.startsWith('+') && !line.startsWith("+++") ->
                                    if (MaterialTheme.colorScheme.background.luminance() < 0.5f)
                                        Color(0xFFA6DAB5)
                                    else Color(0xFF28713D)
                                line.startsWith('-') && !line.startsWith("---") ->
                                    MaterialTheme.colorScheme.error
                                line.startsWith("@@") -> MaterialTheme.colorScheme.primary
                                else -> MaterialTheme.colorScheme.onSurfaceVariant
                            },
                    )
                }
            }
        }
        if (lines.size > limit)
            TextButton(onClick = { limit += 200 }) { Text("Afficher la suite du diff") }
    }
}

@Composable
private fun PlanView(tasks: List<JsonElement>) {
    fun done(t: JsonElement) =
        t.asObject()?.get("completed") == JsonPrimitive(true) ||
            t.asObject().string("status") in listOf("completed", "done")
    Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(
            "${tasks.count(::done)} / ${tasks.size} étapes terminées",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
        )
        if (tasks.isNotEmpty())
            LinearProgressIndicator(
                progress = { tasks.count(::done).toFloat() / tasks.size },
                modifier = Modifier.fillMaxWidth(),
            )
        tasks.forEach { value ->
            val task = value.asObject()
            val complete = done(value)
            Row(
                horizontalArrangement = Arrangement.spacedBy(10.dp),
                verticalAlignment = Alignment.Top,
            ) {
                Icon(
                    if (complete) Icons.Default.Check
                    else if (task.string("status") == "in_progress") Icons.Default.PlayArrow
                    else CairnIcons.Right,
                    null,
                    Modifier.size(18.dp),
                    tint =
                        if (complete) MaterialTheme.colorScheme.primary
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    task.string("text").ifBlank { task.string("step") },
                    style = MaterialTheme.typography.bodyMedium,
                    textDecoration = if (complete) TextDecoration.LineThrough else null,
                )
            }
        }
    }
}

@Composable
private fun McpResult(item: JsonObject?) {
    val args =
        item?.get("arguments")?.let {
            if (it is JsonPrimitive && it.isString)
                runCatching { wireJson.parseToJsonElement(it.content) }.getOrDefault(it)
            else it
        }
    val name = item.string("tool").substringAfterLast("__")
    if (name in listOf("update_plan", "plan") && args.asObject()?.get("plan") is JsonArray)
        PlanView(args.asObject()!!["plan"].asArray().orEmpty())
    else if (name in listOf("exec_command", "shell_command") && args.asObject() != null)
        CodeBlock(
            "Commande",
            args.asObject().string("cmd").ifBlank { args.asObject().string("command") },
        )
    if (args != null)
        Disclosure(
            "Paramètres de l’outil",
            initiallyOpen = name !in listOf("update_plan", "plan", "exec_command", "shell_command"),
        ) {
            StructuredData(args)
        }
    val result = item?.get("result")
    val blocks = result.asObject()?.get("content").asArray()
    if (!blocks.isNullOrEmpty()) {
        blocks.forEach { block ->
            val part = block.asObject()
            when (part.string("type")) {
                "text" -> ResultContent(part.string("text"))
                "image" -> ReturnedImage(part)
                "resource_link" ->
                    ResultLink(
                        part.string("name").ifBlank { part.string("uri") },
                        part.string("uri"),
                    )
                "resource" -> {
                    val resource = part?.get("resource").asObject()
                    if (resource.string("text").isNotBlank()) ResultContent(resource.string("text"))
                    else Text(resource.string("mimeType").ifBlank { "Ressource jointe" })
                    if (resource.string("uri").isNotBlank())
                        ResultLink("Ouvrir la ressource", resource.string("uri"))
                }
                else ->
                    if (part != null)
                        StructuredData(
                            JsonObject(part.filterKeys { it !in listOf("data", "blob") })
                        )
            }
        }
        result.asObject()?.get("structuredContent")?.let { StructuredData(it) }
    } else if (result is JsonPrimitive && result.isString) ResultContent(result.content)
    else if (result != null && result != JsonNull) StructuredData(result)
    item
        ?.get("error")
        ?.takeIf { it != JsonNull && it != JsonPrimitive(false) }
        ?.let {
            Text(
                "Erreur de l’outil",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.titleSmall,
            )
            StructuredData(it)
        }
}

@Composable
private fun ReturnedImage(part: JsonObject?) {
    var bitmap by remember(part) { mutableStateOf<android.graphics.Bitmap?>(null) }
    LaunchedEffect(part) {
        bitmap =
            withContext(Dispatchers.IO) {
                runCatching {
                    val data = part.string("data")
                    require(data.length <= 4_000_000)
                    val bytes = android.util.Base64.decode(data, android.util.Base64.DEFAULT)
                    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                    BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
                    require(bounds.outWidth > 0 && bounds.outHeight > 0)
                    BitmapFactory.decodeByteArray(
                        bytes,
                        0,
                        bytes.size,
                        BitmapFactory.Options().apply {
                            inSampleSize =
                                Integer.highestOneBit(
                                    (maxOf(bounds.outWidth, bounds.outHeight) / 1024).coerceAtLeast(
                                        1
                                    )
                                )
                        },
                    )
                }
                    .getOrNull()
            }
    }
    bitmap?.let {
        Image(
            it.asImageBitmap(),
            "Image retournée par l’outil",
            Modifier.fillMaxWidth().heightIn(max = 220.dp),
        )
    }
        ?: Text(
            "Image jointe · ${part.string("mimeType")}",
            style = MaterialTheme.typography.bodySmall,
        )
}

@Composable
private fun ResultLink(title: String, url: String) {
    val open = LocalArtifactLinks.current
    val context = LocalContext.current
    val artifact = url.startsWith("/api/runs/") || url.contains("/artifacts/")
    if (artifact)
        TextButton(
            onClick = {
                if (!open(url)) {
                    val uri = url.toUri()
                    if (uri.scheme in listOf("http", "https") && uri.host != null)
                        runCatching { CustomTabsIntent.Builder().build().launchUrl(context, uri) }
                    else
                        android.widget.Toast.makeText(
                                context,
                                "Fichier indisponible dans cette conversation",
                                android.widget.Toast.LENGTH_SHORT,
                            )
                            .show()
                }
            }
        ) {
            Icon(CairnIcons.File, null, Modifier.size(16.dp))
            Spacer(Modifier.width(8.dp))
            Text(title, maxLines = 2)
        }
    else ExternalButton(title, url)
}

@Composable
internal fun ResultContent(source: String, depth: Int = 0) {
    val parts = remember(source) { resultParts(source) }
    parts.forEach { part ->
        when (part) {
            is ResultPart.Text ->
                if (part.value.isNotBlank()) {
                    if (part.value.length > 8000) CodeBlock("Résultat", part.value)
                    else Markdown(part.value)
                }
            is ResultPart.Data -> StructuredData(part.value, depth)
            is ResultPart.Incomplete -> {
                Text("Résultat incomplet", style = MaterialTheme.typography.titleSmall)
                Text(
                    "La sortie enregistrée s’interrompt avant la fin. Le contenu partiel est disponible dans les détails techniques.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
}

@Composable
internal fun StructuredData(value: JsonElement, depth: Int = 0, field: String = "") {
    if (depth > 6) {
        Text(
            "Contenu détaillé disponible dans la source.",
            style = MaterialTheme.typography.bodySmall,
        )
        return
    }
    when (value) {
        is JsonObject ->
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                val title =
                    when {
                        value["jobs"] is JsonArray -> "Vérifications du workflow"
                        value["files"] is JsonArray &&
                            (value.containsKey("headRefOid") || value.containsKey("baseRefOid")) ->
                            "Pull request"
                        else -> ""
                    }
                if (title.isNotBlank()) Text(title, style = MaterialTheme.typography.titleSmall)
                var limit by rememberSaveable { mutableIntStateOf(16) }
                value.entries.take(limit).forEach { (key, item) ->
                    if (item is JsonObject || item is JsonArray)
                        Disclosure(
                            "${readableField(key)} · ${if(item is JsonArray) item.size else (item as JsonObject).size}",
                            initiallyOpen =
                                depth < 2 && key in listOf("jobs", "files", "results", "plan"),
                        ) {
                            StructuredData(item, depth + 1, key)
                        }
                    else if (key in listOf("url", "html_url", "uri") && item.plain().isNotBlank())
                        ResultLink("Ouvrir le lien", item.plain())
                    else
                        Row(
                            Modifier.fillMaxWidth().padding(vertical = 4.dp),
                            horizontalArrangement = Arrangement.spacedBy(12.dp),
                        ) {
                            Text(
                                readableField(key),
                                Modifier.weight(0.38f),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Box(Modifier.weight(0.62f)) {
                                if (key.lowercase() in listOf("status", "state", "conclusion"))
                                    Outcome(item.plain())
                                else ScalarData(item, depth)
                            }
                        }
                }
                if (value.size > limit)
                    TextButton(onClick = { limit += 30 }) {
                        Text("Afficher ${value.size-limit} autres champs")
                    }
                if (value.isEmpty()) Text("Aucun champ", style = MaterialTheme.typography.bodySmall)
            }
        is JsonArray ->
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                var limit by rememberSaveable { mutableIntStateOf(8) }
                val checks = value.filter { it.asObject()?.containsKey("conclusion") == true }
                if (checks.isNotEmpty())
                    Text(
                        "${checks.count { it.asObject().string("conclusion") in listOf("success","succeeded","passed") }} / ${checks.size} vérifications réussies",
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.primary,
                    )
                value.take(limit).forEachIndexed { index, item ->
                    if (item is JsonObject)
                        Surface(
                            color = MaterialTheme.colorScheme.surfaceVariant,
                            shape = RoundedCornerShape(10.dp),
                        ) {
                            Column(
                                Modifier.fillMaxWidth().padding(10.dp),
                                verticalArrangement = Arrangement.spacedBy(6.dp),
                            ) {
                                val key =
                                    listOf("name", "title", "path", "id").firstOrNull {
                                        item[it] is JsonPrimitive
                                    }
                                Text(
                                    key?.let { item[it].plain() } ?: "Élément ${index+1}",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                StructuredData(
                                    JsonObject(item.filterKeys { it != key }),
                                    depth + 1,
                                    field,
                                )
                            }
                        }
                    else if (item is JsonArray)
                        Disclosure("Élément ${index+1}") { StructuredData(item, depth + 1, field) }
                    else if (field == "files") PathRow(item.plain()) else ScalarData(item, depth)
                }
                if (value.size > limit)
                    TextButton(onClick = { limit += 50 }) {
                        Text("Afficher ${value.size-limit} autres éléments")
                    }
                if (value.isEmpty())
                    Text("Aucun élément", style = MaterialTheme.typography.bodySmall)
            }
        else -> ScalarData(value, depth)
    }
}

@Composable
private fun ScalarData(value: JsonElement, depth: Int) {
    val text =
        when (value) {
            JsonNull -> "Non renseigné"
            JsonPrimitive(true) -> "Oui"
            JsonPrimitive(false) -> "Non"
            else -> value.plain().ifEmpty { "Vide" }
        }
    if (
        value is JsonPrimitive &&
            value.isString &&
            text.trimStart().firstOrNull() in listOf('{', '[')
    ) {
        if (depth < 6) ResultContent(text, depth + 1)
        else
            Text(
                "Contenu détaillé disponible dans la source.",
                style = MaterialTheme.typography.bodySmall,
            )
    } else Text(text.take(4000), style = MaterialTheme.typography.bodySmall)
}
