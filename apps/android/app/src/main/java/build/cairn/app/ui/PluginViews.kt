@file:OptIn(ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

private fun JsonObject.text(key: String): String =
    (this[key] as? JsonPrimitive)?.contentOrNull.orEmpty()

private fun JsonObject.list(key: String): List<JsonObject> =
    (this[key] as? JsonArray).orEmpty().mapNotNull { it as? JsonObject }

private fun tone(name: String) =
    when (name) {
        "success" -> PluginTone.OK
        "warning" -> PluginTone.WARNING
        "danger" -> PluginTone.ATTENTION
        else -> PluginTone.MUTED
    }

/**
 * A package's panel for one instance: its declarative view, then whatever its actions return.
 * Errors stay in the panel; an expired session still signs out.
 */
@Composable
internal fun PluginPanel(vm: CairnViewModel, state: Workspace, path: String, name: String) {
    var nodes by remember(path) { mutableStateOf<JsonArray?>(null) }
    var error by remember(path) { mutableStateOf<String?>(null) }
    var busy by remember(path) { mutableStateOf(false) }
    var confirming by remember(path) { mutableStateOf<JsonObject?>(null) }
    val scope = rememberCoroutineScope()
    suspend fun guard(block: suspend () -> Unit) {
        busy = true
        error = null
        try {
            block()
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (e is ApiException && e.status == 401) vm.report(e)
            error = e.message ?: "Le plugin n’a pas répondu."
        } finally {
            busy = false
        }
    }
    LaunchedEffect(path) {
        guard { nodes = vm.api.get<JsonObject>("$path/view")["view"] as? JsonArray }
    }
    fun act(action: String, payload: JsonElement, values: JsonObject) {
        scope.launch {
            guard {
                val result =
                    vm.api.send<JsonObject>(
                        "POST",
                        "$path/actions",
                        buildJsonObject {
                            put("action", action)
                            put("payload", payload)
                            put("values", values)
                        },
                    )
                (result["view"] as? JsonArray)?.let { nodes = it }
                result.text("notice").takeIf { it.isNotBlank() }?.let(vm::notify)
            }
        }
    }
    fun press(button: JsonObject) {
        if (button.text("confirm").isNotBlank()) confirming = button
        else act(button.text("action"), button["payload"] ?: JsonNull, JsonObject(emptyMap()))
    }
    Column(
        Modifier.fillMaxWidth().testTag("plugin-panel"),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
        if (busy && nodes == null) LinearProgressIndicator(Modifier.fillMaxWidth())
        error?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
        nodes?.let { PluginViewNodes(it, busy || state.busy, ::press, ::act) }
    }
    confirming?.let { button ->
        Confirm(
            button.text("label"),
            button.text("confirm"),
            busy,
            null,
            dismiss = { confirming = null },
        ) {
            confirming = null
            act(button.text("action"), button["payload"] ?: JsonNull, JsonObject(emptyMap()))
        }
    }
}

@Composable
private fun PluginViewNodes(
    nodes: JsonArray,
    busy: Boolean,
    press: (JsonObject) -> Unit,
    act: (String, JsonElement, JsonObject) -> Unit,
) {
    val context = LocalContext.current
    nodes
        .mapNotNull { it as? JsonObject }
        .forEach { node ->
            when (node.text("type")) {
                "section" ->
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (node.text("title").isNotBlank())
                            Text(node.text("title"), style = MaterialTheme.typography.titleSmall)
                        if (node.text("description").isNotBlank())
                            Text(
                                node.text("description"),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        PluginViewNodes(
                            (node["children"] as? JsonArray) ?: JsonArray(emptyList()),
                            busy,
                            press,
                            act,
                        )
                    }
                "text" ->
                    Text(
                        node.text("text"),
                        style = MaterialTheme.typography.bodyMedium,
                        color =
                            if (node.text("tone").let { it.isBlank() || it == "neutral" })
                                MaterialTheme.colorScheme.onSurface
                            else MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                "alert" -> {
                    val (container, content) =
                        when (tone(node.text("tone"))) {
                            PluginTone.OK -> signal.success.copy(alpha = 0.14f) to signal.success
                            PluginTone.WARNING -> signal.warningSoft to signal.warning
                            PluginTone.ATTENTION -> signal.attentionSoft to signal.attention
                            PluginTone.MUTED ->
                                MaterialTheme.colorScheme.primaryContainer to
                                    MaterialTheme.colorScheme.primary
                        }
                    Column(
                        Modifier.fillMaxWidth()
                            .clip(RoundedCornerShape(12.dp))
                            .background(container)
                            .padding(12.dp)
                    ) {
                        if (node.text("title").isNotBlank())
                            Text(
                                node.text("title"),
                                style = MaterialTheme.typography.titleSmall,
                                color = content,
                            )
                        Text(
                            node.text("text"),
                            style = MaterialTheme.typography.bodySmall,
                            color = content,
                        )
                    }
                }
                "badge" -> ToneChip(PluginBadge(node.text("label"), tone(node.text("tone"))))
                "facts" ->
                    node.list("items").forEach { item ->
                        PanelFact(item.text("label"), item.text("value").ifBlank { "—" })
                    }
                "list" -> {
                    val items = node.list("items")
                    if (items.isEmpty())
                        Text(
                            node.text("empty").ifBlank { "Rien pour l’instant." },
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    items.forEach { item ->
                        Row(
                            Modifier.fillMaxWidth()
                                .clip(RoundedCornerShape(12.dp))
                                .background(MaterialTheme.colorScheme.surfaceContainer)
                                .padding(start = 12.dp, end = 4.dp, top = 8.dp, bottom = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(
                                    item.text("title"),
                                    style = MaterialTheme.typography.titleSmall,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                                if (item.text("subtitle").isNotBlank())
                                    Text(
                                        item.text("subtitle"),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        maxLines = 1,
                                        overflow = TextOverflow.Ellipsis,
                                    )
                            }
                            (item["badge"] as? JsonObject)?.let {
                                ToneChip(PluginBadge(it.text("label"), tone(it.text("tone"))))
                            }
                            item.list("actions").forEach { button ->
                                PanelButton(button, busy, press)
                            }
                        }
                    }
                }
                "progress" -> {
                    val value = (node["value"] as? JsonPrimitive)?.doubleOrNull ?: 0.0
                    val max = (node["max"] as? JsonPrimitive)?.doubleOrNull ?: 1.0
                    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Row {
                            Text(
                                node.text("label"),
                                Modifier.weight(1f),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                            Text(
                                "${value.toBigDecimal().stripTrailingZeros().toPlainString()} / ${max.toBigDecimal().stripTrailingZeros().toPlainString()}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        LinearProgressIndicator(
                            progress = { (value / max).toFloat().coerceIn(0f, 1f) },
                            modifier = Modifier.fillMaxWidth().height(6.dp).clip(CircleShape),
                            drawStopIndicator = {},
                        )
                    }
                }
                "code" ->
                    Surface(
                        shape = RoundedCornerShape(12.dp),
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Box(Modifier.padding(10.dp)) { Code(node.text("text")) }
                    }
                "link" ->
                    TextButton(onClick = { runCatching { browse(context, node.text("url")) } }) {
                        Text(node.text("label"))
                    }
                "actions" ->
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        node.list("children").forEach { button -> PanelButton(button, busy, press) }
                    }
                "button" -> PanelButton(node, busy, press)
                "form" -> PanelForm(node, busy) { action, values -> act(action, JsonNull, values) }
            }
        }
}

@Composable
private fun PanelFact(label: String, value: String) {
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .padding(horizontal = 14.dp, vertical = 10.dp)
    ) {
        Eyebrow(label)
        Text(value, Modifier.padding(top = 2.dp), style = MaterialTheme.typography.titleSmall)
    }
}

@Composable
private fun PanelButton(button: JsonObject, busy: Boolean, press: (JsonObject) -> Unit) {
    val danger = button.text("style") == "danger"
    if (button.text("style") == "primary")
        SignalButton(button.text("label"), height = 40.dp, enabled = !busy) { press(button) }
    else
        TextButton(onClick = { press(button) }, enabled = !busy) {
            Text(
                button.text("label"),
                color =
                    if (danger) MaterialTheme.colorScheme.error
                    else MaterialTheme.colorScheme.primary,
            )
        }
}

/** A view's form: the same declarative fields as instance settings, sent to an action. */
@Composable
private fun PanelForm(node: JsonObject, busy: Boolean, submit: (String, JsonObject) -> Unit) {
    val fields =
        remember(node) {
            runCatching {
                    wireJson.decodeFromJsonElement<List<PluginSetting>>(
                        node["fields"] ?: JsonArray(emptyList())
                    )
                }
                .getOrDefault(emptyList())
        }
    val initial = (node["values"] as? JsonObject).orEmpty()
    var values by
        remember(node) {
            mutableStateOf(fields.associate { it.key to formValue(it, initial[it.key]) })
        }
    var secrets by remember(node) { mutableStateOf(mapOf<String, String>()) }
    val action = (node["submit"] as? JsonObject)?.text("action").orEmpty()
    Column(
        Modifier.fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(MaterialTheme.colorScheme.surfaceContainer)
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        PluginFields(
            fields,
            values,
            { key, value -> values = values + (key to value) },
            secrets,
            { key, value -> secrets = secrets + (key to value) },
            enabled = !busy,
        )
        SignalButton(
            (node["submit"] as? JsonObject)?.text("label").orEmpty(),
            height = 40.dp,
            enabled = !busy,
        ) {
            // A form's secret fields go to the package like its other values.
            val output =
                pluginSettings(fields, values) +
                    secrets.filterValues { it.isNotBlank() }.mapValues { JsonPrimitive(it.value) }
            secrets = emptyMap()
            submit(action, JsonObject(output))
        }
    }
}
