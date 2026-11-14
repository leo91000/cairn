@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.layout.ExperimentalLayoutApi::class,
)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal data class RuleResourceSource(
    val pluginId: String,
    val instanceId: String,
    val resourceType: String,
)

internal fun formulaRuleValue(value: String) =
    ruleObject("type" to JsonPrimitive("formula"), "expression" to JsonPrimitive(value))

@Composable
internal fun RuleValueEditor(
    label: String,
    value: JsonElement?,
    variables: List<RuleVariable>,
    change: (JsonObject) -> Unit,
    expects: String = "text",
    multiline: Boolean = false,
    schema: JsonObject? = null,
    resourceSource: RuleResourceSource? = null,
    vm: CairnViewModel? = null,
) {
    val held = value as? JsonObject ?: templateValue()
    val mode = held.text("type").ifBlank { "template" }
    var picking by remember { mutableStateOf(false) }
    var browsing by remember { mutableStateOf(false) }
    val text =
        when (mode) {
            "formula" -> held.text("expression")
            "literal" ->
                (held["value"] as? JsonPrimitive)?.contentOrNull ?: held["value"].toString()
            else -> held.text("text")
        }
    val modes =
        if (expects in setOf("field", "formula")) listOf("formula" to "Variable ou formule")
        else if (expects in setOf("number", "integer", "boolean"))
            listOf("literal" to "Valeur", "formula" to "Formule")
        else listOf("template" to "Texte", "literal" to "Valeur", "formula" to "Formule")
    Column(verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            modes.forEach { (kind, title) ->
                FilterChip(
                    selected = mode == kind,
                    onClick = {
                        change(
                            when (kind) {
                                "literal" ->
                                    literalRuleValue(
                                        if (schema != null) defaultRuleLiteral(schema)
                                        else
                                            when (expects) {
                                                "number" ->
                                                    JsonPrimitive(text.toDoubleOrNull() ?: 0.0)
                                                "boolean" -> JsonPrimitive(true)
                                                "json" ->
                                                    if (
                                                        schema != null &&
                                                            schemaKind(schema, JsonNull) == "array"
                                                    )
                                                        JsonArray(emptyList())
                                                    else JsonObject(emptyMap())
                                                else -> JsonPrimitive(text)
                                            }
                                    )
                                "formula" ->
                                    formulaRuleValue(
                                        if (
                                            mode == "template" &&
                                                text.startsWith("{{") &&
                                                text.endsWith("}}")
                                        )
                                            text.substring(2, text.length - 2).trim()
                                        else if (mode == "literal") held["value"].toString()
                                        else JsonPrimitive(text).toString()
                                    )
                                else -> templateValue(if (mode == "formula") "{{$text}}" else text)
                            }
                        )
                    },
                    label = { Text(title, style = MaterialTheme.typography.labelSmall) },
                )
            }
        }
        if (mode == "literal" && schema != null)
            RuleLiteralEditor(
                schema,
                held["value"] ?: JsonNull,
                change = { change(literalRuleValue(it)) },
            )
        else if (mode == "literal" && expects in setOf("number", "integer"))
            RuleNumericField(held["value"] ?: JsonNull, expects == "integer", label) {
                change(literalRuleValue(it))
            }
        else if (mode == "literal" && expects == "boolean")
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                listOf(true to "Oui", false to "Non").forEach { (flag, title) ->
                    FilterChip(
                        selected = held["value"].flag() == flag,
                        onClick = { change(literalRuleValue(JsonPrimitive(flag))) },
                        label = { Text(title) },
                    )
                }
            }
        else
            OutlinedTextField(
                text,
                { input ->
                    change(
                        when (mode) {
                            "formula" -> formulaRuleValue(input)
                            "literal" ->
                                literalRuleValue(
                                    if (expects in setOf("number", "integer"))
                                        ruleNumber(input, expects == "integer")
                                    else if (expects == "json")
                                        runCatching { wireJson.parseToJsonElement(input) }
                                            .getOrElse { JsonPrimitive(input) }
                                    else JsonPrimitive(input)
                                )
                            else -> templateValue(input)
                        }
                    )
                },
                Modifier.fillMaxWidth(),
                placeholder = {
                    Text(
                        if (mode == "formula") "Choisir une variable ou écrire une formule"
                        else "Texte avec {{variables}}"
                    )
                },
                minLines = if (multiline) 3 else 1,
                maxLines = if (multiline) 8 else 3,
                textStyle =
                    if (mode == "formula")
                        MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace)
                    else MaterialTheme.typography.bodyMedium,
                shape = RoundedCornerShape(14.dp),
            )
        val typed = expects in setOf("number", "integer", "boolean")
        val literal = held["value"]
        val wrongType =
            typed &&
                mode != "formula" &&
                (mode != "literal" ||
                    if (expects == "boolean")
                        literal !is JsonPrimitive ||
                            literal.isString ||
                            literal.booleanOrNull == null
                    else
                        literal !is JsonPrimitive ||
                            literal.isString ||
                            literal.doubleOrNull == null)
        if (wrongType)
            Text(
                "Choisissez une valeur ${if (expects == "boolean") "vraie ou fausse" else "numérique"} ou une formule de ce type.",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { picking = true }) { Text("+ Insérer une variable") }
            if (resourceSource != null && vm != null)
                TextButton(onClick = { browsing = true }) { Text("Choisir une ressource") }
        }
        if (mode == "template" && text.contains("{{"))
            Text(
                "Les parties entre {{…}} seront remplacées par les données de l’événement.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
            )
    }
    if (picking) {
        var search by remember { mutableStateOf("") }
        ModalBottomSheet(onDismissRequest = { picking = false }) {
            Column(Modifier.fillMaxWidth().fillMaxHeight(0.8f).padding(horizontal = 20.dp)) {
                Text("Insérer une variable", style = MaterialTheme.typography.titleLarge)
                OutlinedTextField(
                    search,
                    { search = it },
                    Modifier.fillMaxWidth().padding(vertical = 12.dp),
                    label = { Text("Rechercher un champ") },
                )
                LazyColumn {
                    items(
                        variables.filter {
                            "${it.label} ${it.expression} ${it.description}".contains(search, true)
                        },
                        key = { "${it.group}:${it.expression}" },
                    ) { variable ->
                        ListItem(
                            headlineContent = { Text(variable.label) },
                            supportingContent = {
                                Column {
                                    Text("${variable.group} · ${variable.description}")
                                    variable.example?.let {
                                        Text(it, fontFamily = FontFamily.Monospace)
                                    }
                                }
                            },
                            modifier = Modifier.fillMaxWidth(),
                        )
                        TextButton(
                            onClick = {
                                change(
                                    if (mode == "formula" || expects == "field")
                                        formulaRuleValue(
                                            if (expects == "field") variable.expression
                                            else text + variable.expression
                                        )
                                    else templateValue(text + "{{${variable.expression}}}")
                                )
                                picking = false
                            }
                        ) {
                            Text("Utiliser ${variable.label}")
                        }
                    }
                }
            }
        }
    }
    if (browsing && resourceSource != null && vm != null)
        RuleResourceSheet(
            vm,
            resourceSource,
            { resource ->
                change(literalRuleValue(JsonPrimitive(resource.text("id"))))
                browsing = false
            },
            { browsing = false },
        )
}

private fun schemaKind(schema: JsonObject, value: JsonElement): String {
    val kind = schema["type"]
    return when (kind) {
        is JsonPrimitive -> kind.content
        is JsonArray ->
            kind
                .firstOrNull { (it as? JsonPrimitive)?.content != "null" }
                ?.jsonPrimitive
                ?.content
                .orEmpty()
        else ->
            if (schema["properties"] != null || value is JsonObject) "object"
            else if (schema["items"] != null || value is JsonArray) "array" else "string"
    }
}

@Composable
internal fun RuleLiteralEditor(
    schema: JsonObject,
    value: JsonElement,
    change: (JsonElement) -> Unit,
    depth: Int = 0,
) {
    val options = (schema["enum"] as? JsonArray).orEmpty()
    val kind = schemaKind(schema, value)
    when {
        options.isNotEmpty() ->
            Choice(
                "Valeur",
                value.toString(),
                options.map {
                    it.toString() to ((it as? JsonPrimitive)?.contentOrNull ?: it.toString())
                },
            ) { key ->
                change(options.first { it.toString() == key })
            }
        kind == "boolean" -> Toggle("Oui", value.flag() == true) { change(JsonPrimitive(it)) }
        kind == "object" && depth < 6 -> {
            val fields = schema.obj("properties").orEmpty()
            val current = value as? JsonObject ?: JsonObject(emptyMap())
            Column(
                Modifier.fillMaxWidth().padding(start = if (depth == 0) 0.dp else 10.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                fields.forEach { (name, definition) ->
                    Text(
                        definition.text("title").ifBlank { name },
                        style = MaterialTheme.typography.labelMedium,
                    )
                    RuleLiteralEditor(
                        definition as? JsonObject ?: JsonObject(emptyMap()),
                        current[name] ?: JsonNull,
                        { change(current.patch(name to it)) },
                        depth + 1,
                    )
                    if (definition.text("description").isNotBlank())
                        Text(
                            definition.text("description"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                }
            }
        }
        kind == "array" && depth < 6 -> {
            val values = value as? JsonArray ?: JsonArray(emptyList())
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                values.forEachIndexed { index, item ->
                    Surface(
                        shape = RoundedCornerShape(14.dp),
                        color = MaterialTheme.colorScheme.surfaceContainer,
                    ) {
                        Column(Modifier.padding(10.dp)) {
                            RuleLiteralEditor(
                                schema.obj("items").orEmpty(),
                                item,
                                { next ->
                                    change(
                                        JsonArray(values.toMutableList().apply { set(index, next) })
                                    )
                                },
                                depth + 1,
                            )
                            TextButton(
                                onClick = {
                                    change(JsonArray(values.filterIndexed { at, _ -> at != index }))
                                }
                            ) {
                                Text("Retirer cet élément")
                            }
                        }
                    }
                }
                OutlinedButton(
                    onClick = {
                        change(
                            JsonArray(values + defaultRuleLiteral(schema.obj("items").orEmpty()))
                        )
                    }
                ) {
                    Text("+ Ajouter un élément")
                }
            }
        }
        kind in setOf("number", "integer") ->
            RuleNumericField(value, kind == "integer", "Valeur") { change(it) }
        else -> {
            val text = (value as? JsonPrimitive)?.contentOrNull.orEmpty()
            OutlinedTextField(
                text,
                { raw ->
                    change(
                        if (kind in setOf("number", "integer"))
                            raw.toDoubleOrNull()?.let(::JsonPrimitive) ?: JsonPrimitive(raw)
                        else JsonPrimitive(raw)
                    )
                },
                Modifier.fillMaxWidth(),
                singleLine = true,
                shape = RoundedCornerShape(12.dp),
            )
        }
    }
}

private fun ruleNumber(raw: String, integer: Boolean): JsonElement {
    raw.toLongOrNull()?.let {
        return JsonPrimitive(it)
    }
    if (!integer)
        raw.toDoubleOrNull()
            ?.takeIf { it.isFinite() }
            ?.let {
                return JsonPrimitive(it)
            }
    return JsonPrimitive(raw)
}

@Composable
private fun RuleNumericField(
    value: JsonElement,
    integer: Boolean,
    label: String,
    change: (JsonElement) -> Unit,
) {
    var draft by
        remember(label, integer) {
            mutableStateOf((value as? JsonPrimitive)?.contentOrNull.orEmpty())
        }
    var emitted by remember(label, integer) { mutableStateOf(value) }
    LaunchedEffect(value) {
        if (value != emitted) {
            draft = (value as? JsonPrimitive)?.contentOrNull.orEmpty()
            emitted = value
        }
    }
    OutlinedTextField(
        draft,
        { raw ->
            draft = raw
            emitted = ruleNumber(raw, integer)
            change(emitted)
        },
        Modifier.fillMaxWidth(),
        singleLine = true,
        keyboardOptions =
            androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = androidx.compose.ui.text.input.KeyboardType.Decimal
            ),
        isError =
            draft.isNotBlank() &&
                (if (integer) draft.toLongOrNull() == null else draft.toDoubleOrNull() == null),
        shape = RoundedCornerShape(12.dp),
    )
}

@Composable
internal fun RuleResourceSheet(
    vm: CairnViewModel,
    source: RuleResourceSource,
    choose: (JsonObject) -> Unit,
    close: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var page by remember { mutableStateOf<Long?>(1) }
    var items by remember { mutableStateOf<List<JsonObject>>(emptyList()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    suspend fun load(next: Long = 1) {
        loading = true
        error = ""
        try {
            val path =
                "/plugins/${segment(source.pluginId)}/connections/${segment(source.instanceId)}/resources?resourceType=${segment(source.resourceType)}&query=${segment(query)}&page=$next"
            val result = vm.api.pluginOperation(path, method = "GET")
            val found = result.list("items").map { it.jsonObject }
            items = if (next == 1L) found else (items + found).distinctBy { it.text("id") }
            page = result.jsonObject["nextPage"]?.jsonPrimitive?.longOrNull
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message.orEmpty()
        } finally {
            loading = false
        }
    }
    LaunchedEffect(query, source) {
        delay(250)
        load()
    }
    ModalBottomSheet(onDismissRequest = close) {
        Column(Modifier.fillMaxWidth().fillMaxHeight(0.85f).padding(horizontal = 20.dp)) {
            Text("Choisir ${source.resourceType}", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(
                query,
                { query = it },
                Modifier.fillMaxWidth().padding(vertical = 12.dp),
                label = { Text("Rechercher par nom") },
                singleLine = true,
            )
            if (error.isNotBlank()) Text(error, color = MaterialTheme.colorScheme.error)
            if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
            LazyColumn(Modifier.weight(1f)) {
                items(items, key = { it.text("id") }) { resource ->
                    TextButton(onClick = { choose(resource) }, modifier = Modifier.fillMaxWidth()) {
                        Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                            Text(resource.text("name").ifBlank { resource.text("id") })
                            if (resource.obj("parent") != null)
                                Text(
                                    resource.obj("parent").text("resourceType"),
                                    style = MaterialTheme.typography.bodySmall,
                                )
                        }
                    }
                }
                if (page != null)
                    item {
                        TextButton(
                            onClick = { scope.launch { load(page!!) } },
                            enabled = !loading,
                        ) {
                            Text("Voir la suite")
                        }
                    }
            }
        }
    }
}
