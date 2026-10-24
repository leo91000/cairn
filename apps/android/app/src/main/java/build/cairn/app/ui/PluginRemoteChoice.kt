package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.ensureActive
import kotlinx.serialization.json.*

/** An editor binds credentials/scope; a field can only ask for its own choices. */
internal data class PluginChoiceLookup(
    val api: CairnApi,
    val path: String,
    val body: JsonObject,
    val generation: String = "",
) {
    fun context(field: String): JsonObject = buildJsonObject {
        put("path", path)
        put("generation", generation)
        val settings = (body["settings"] as? JsonObject).orEmpty().filterKeys { it != field }
        put("body", JsonObject(body + ("settings" to JsonObject(settings))))
    }

    suspend fun request(lookup: PluginFormLookup): PluginForm {
        val input = JsonObject(body + ("lookup" to wireJson.encodeToJsonElement(lookup)))
        val result = decodePluginForm(api.pluginOperation(path, input))
        check(result.lookup == lookup) {
            "Le serveur n’a pas confirmé la recherche. Mettez Cairn à jour puis réessayez."
        }
        return result
    }
}

@Composable
internal fun PluginRemoteChoice(
    field: PluginSetting,
    value: String,
    change: (String) -> Unit,
    lookup: PluginChoiceLookup?,
    enabled: Boolean,
) {
    // These keys and credentials live only in memory, never in a saveable draft.
    val context = field to lookup?.context(field.key)
    var query by remember(context) { mutableStateOf("") }
    var cursor by remember(context) { mutableStateOf<String?>(null) }
    var attempt by remember(context) { mutableIntStateOf(0) }
    var options by remember(context) { mutableStateOf(emptyList<PluginOption>()) }
    var labels by remember(context) { mutableStateOf(emptyMap<String, PluginOption>()) }
    var page by remember(context) { mutableStateOf<PluginChoicePage?>(null) }
    var loading by remember(context) { mutableStateOf(true) }
    var error by remember(context) { mutableStateOf<String?>(null) }
    val selected =
        if (field.type == "multiselect")
            multiselect(value).mapNotNull { it.jsonPrimitive.contentOrNull }
        else value.takeIf { it.isNotEmpty() }?.let(::listOf).orEmpty()
    fun rememberNames(ids: List<String>) {
        labels =
            ids.take(100).associateWith { id ->
                options.find { it.value == id } ?: labels[id] ?: PluginOption(id, id)
            }
    }
    fun choose(ids: List<String>) {
        rememberNames(ids)
        change(
            if (field.type == "multiselect") JsonArray(ids.map(::JsonPrimitive)).toString()
            else ids.firstOrNull().orEmpty()
        )
    }
    LaunchedEffect(field, lookup, query, cursor, attempt, enabled) {
        loading = enabled
        if (!enabled) return@LaunchedEffect
        error = null
        options = emptyList()
        page = null
        delay(250)
        try {
            check(lookup != null) { "Ce formulaire ne permet pas la recherche distante." }
            val result = lookup.request(PluginFormLookup(field.key, query, cursor))
            currentCoroutineContext().ensureActive()
            val more = result.pages?.get(field.key)
            check(cursor == null || more?.nextCursor != cursor) {
                "Le plugin a renvoyé la même page. Réessayez ou changez la recherche."
            }
            options = result.options[field.key].orEmpty()
            page = more
            rememberNames(selected)
            error = result.errors.values.joinToString(" ").takeIf { it.isNotEmpty() }
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message ?: "La recherche a échoué."
        } finally {
            currentCoroutineContext().ensureActive()
            loading = false
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Field(
            "Rechercher · ${field.title}",
            query,
            {
                val next = it.take(200)
                if (query != next) {
                    query = next
                    cursor = null
                    options = emptyList()
                    page = null
                    loading = true
                }
            },
            enabled = enabled,
        )
        if (selected.isNotEmpty()) {
            Text("Sélection", style = MaterialTheme.typography.labelLarge)
            selected.forEach { id ->
                val label = labels[id]?.label ?: id
                Row(Modifier.fillMaxWidth()) {
                    Text(label, Modifier.weight(1f))
                    if (!field.required || field.type == "multiselect")
                        TextButton(onClick = { choose(selected - id) }, enabled = enabled) {
                            Text("Retirer $label")
                        }
                }
            }
        }
        if (loading) Text("Chargement des choix…", style = MaterialTheme.typography.bodySmall)
        if (field.type == "select")
            ChoiceList(field.title, value, options, enabled && !loading) { choose(listOf(it)) }
        else {
            Text(field.title, style = MaterialTheme.typography.labelLarge)
            options.forEach { option ->
                val checked = option.value in selected
                Row(Modifier.fillMaxWidth()) {
                    Checkbox(
                        checked,
                        { choose(if (it) selected + option.value else selected - option.value) },
                        modifier = Modifier.semantics { contentDescription = option.label },
                        enabled =
                            enabled &&
                                !loading &&
                                !option.disabled &&
                                (checked || selected.size < 100),
                    )
                    Column(Modifier.weight(1f)) {
                        Text(option.label)
                        if (option.description.isNotBlank())
                            Text(option.description, style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        }
        val completeness =
            when {
                page == null -> "Complétude inconnue"
                page?.nextCursor == null -> "Fin des résultats"
                else -> "D’autres choix sont disponibles"
            }
        Text(
            "${options.size} choix sur cette page · $completeness",
            style = MaterialTheme.typography.bodySmall,
        )
        page?.nextCursor?.let { next ->
            TextButton(
                onClick = {
                    cursor = next
                    loading = true
                },
                enabled = enabled && !loading,
            ) {
                Text("Page suivante")
            }
        }
        error?.let { FormError(it) }
        if (error != null || page == null)
            TextButton(onClick = { attempt++ }, enabled = enabled && !loading) {
                Text("Réessayer les choix")
            }
    }
}
