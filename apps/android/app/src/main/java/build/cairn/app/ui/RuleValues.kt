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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

internal data class RuleResourceSource(
    val pluginId: String,
    val instanceId: String,
    val resourceType: String,
    val agentId: String,
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
    resourceLabel: String = "Choisir une ressource",
) {
    val held = value as? JsonObject ?: templateValue()
    val jsonDrafts = ruleValueJsonDrafts(held)
    val storedMode = held.text("type").ifBlank { "template" }
    val mode =
        if (storedMode == "literal" && expects in setOf("field", "formula")) "formula"
        else storedMode
    var literalSelection by
        remember(mode, schema, expects) { mutableStateOf<TextFieldValue?>(null) }
    var picking by remember { mutableStateOf(false) }
    var browsing by remember { mutableStateOf(false) }
    var conversionNotice by remember { mutableStateOf("") }
    val text =
        if (jsonDrafts.containsKey("")) jsonDrafts.obj("").text("text")
        else
            when (mode) {
                "formula" ->
                    if (storedMode == "literal") (held["value"] ?: JsonNull).toString()
                    else held.text("expression")
                "literal" ->
                    if (expects == "json") held["value"].toString()
                    else
                        (held["value"] as? JsonPrimitive)?.contentOrNull ?: held["value"].toString()
                else -> held.text("text")
            }
    var editorInput by remember { mutableStateOf(TextFieldValue(text, TextRange(text.length))) }
    var emittedInput by remember { mutableStateOf<JsonObject?>(null) }
    var emittedExpects by remember { mutableStateOf(expects) }
    LaunchedEffect(held, expects) {
        if (held != emittedInput || expects != emittedExpects) {
            if (editorInput.text != text) editorInput = TextFieldValue(text, TextRange(text.length))
            emittedInput = held
            emittedExpects = expects
        }
    }
    val declaredSchemaKind = schema?.takeIf { it["type"] != null }?.let { schemaKind(it, JsonNull) }
    val modes =
        if (expects in setOf("field", "formula")) listOf("formula" to "Variable ou formule")
        else if (
            expects in setOf("number", "integer", "boolean") ||
                (declaredSchemaKind != null && declaredSchemaKind != "string")
        )
            listOf("literal" to "Valeur", "formula" to "Formule")
        else listOf("template" to "Texte", "literal" to "Valeur", "formula" to "Formule")
    Column(verticalArrangement = Arrangement.spacedBy(6.dp), modifier = Modifier.fillMaxWidth()) {
        Text(label, style = MaterialTheme.typography.labelLarge)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(5.dp)) {
            modes.forEach { (kind, title) ->
                FilterChip(
                    selected = mode == kind,
                    onClick = {
                        if (kind == mode) return@FilterChip
                        val fromFormula = kind == "literal" && mode == "formula"
                        val constant =
                            if (fromFormula) runCatching { parseRuleJsonInput(text) }.getOrNull()
                            else null
                        conversionNotice =
                            if (fromFormula && constant == null)
                                "La formule a été remplacée par une valeur fixe. Vérifiez cette valeur avant d’enregistrer."
                            else ""
                        change(
                            when (kind) {
                                "literal" ->
                                    literalRuleValue(
                                        constant ?: fixedRuleValue(text, expects, schema)
                                    )
                                "formula" ->
                                    formulaRuleValue(
                                        when (mode) {
                                            "template" -> unwrapRuleTemplate(text)
                                            "literal" -> held["value"].toString()
                                            else -> text
                                        }
                                    )
                                else ->
                                    templateValue(
                                        if (mode == "formula") "{{$text}}"
                                        else if (
                                            mode == "literal" &&
                                                (held["value"] as? JsonPrimitive)?.isString == true
                                        )
                                            held["value"]!!.jsonPrimitive.content
                                        else text
                                    )
                            }
                        )
                    },
                    label = { Text(title, style = MaterialTheme.typography.labelSmall) },
                )
            }
        }
        val literalTextSchema = declaredSchemaKind == "string" && schema.list("enum").isEmpty()
        if (mode == "literal" && schema != null && !literalTextSchema)
            RuleLiteralEditor(
                schema,
                held["value"] ?: JsonNull,
                change = { change(literalRuleValue(it)) },
                selectionChanged = { literalSelection = it },
                jsonDrafts = jsonDrafts,
                changeDraft = { value, drafts -> change(literalWithJsonDrafts(value, drafts)) },
            )
        else if (mode == "literal" && expects in setOf("number", "integer"))
            RuleNumericField(
                held["value"] ?: JsonNull,
                expects == "integer",
                label,
                hasProblem = jsonDrafts.isNotEmpty(),
                changeDraft = { value, drafts -> change(literalWithJsonDrafts(value, drafts)) },
            ) {
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
                editorInput,
                { input ->
                    editorInput = input
                    val next =
                        when (mode) {
                            "formula" -> formulaRuleValue(input.text)
                            "literal" ->
                                if (expects == "json") {
                                    val parsed = runCatching {
                                        parseRuleJsonInput(input.text)
                                    }
                                    literalWithJsonDrafts(
                                        parsed.getOrElse { JsonPrimitive(input.text) },
                                        if (parsed.isFailure) jsonInputDraft(input.text)
                                        else JsonObject(emptyMap()),
                                    )
                                } else
                                    literalRuleValue(
                                        if (expects in setOf("number", "integer"))
                                            ruleNumber(input.text, expects == "integer")
                                        else JsonPrimitive(input.text)
                                    )
                            else -> templateValue(input.text)
                        }
                    emittedInput = next
                    emittedExpects = expects
                    change(next)
                },
                Modifier.fillMaxWidth().semantics { contentDescription = label },
                placeholder = {
                    Text(
                        if (mode == "formula") "Choisir une variable ou écrire une formule"
                        else "Texte avec {{variables}}"
                    )
                },
                isError = jsonDrafts.isNotEmpty(),
                minLines = if (multiline) 3 else 1,
                maxLines = if (multiline) 8 else 3,
                textStyle =
                    if (mode == "formula")
                        MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace)
                    else MaterialTheme.typography.bodyMedium,
                shape = RoundedCornerShape(14.dp),
            )
        if (conversionNotice.isNotBlank())
            Text(
                conversionNotice,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                style = MaterialTheme.typography.bodySmall,
            )
        if (jsonDrafts.containsKey(""))
            Text(
                jsonDrafts.obj("").text("message"),
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        val expected =
            declaredSchemaKind
                ?: when (expects) {
                    "text" -> "string"
                    "number",
                    "integer",
                    "boolean" -> expects
                    else -> null
                }
        val literal = held["value"]
        val wrongType =
            expected != null &&
                mode != "formula" &&
                (if (mode == "template") expected != "string"
                else !matchesRuleType(literal, expected))
        if (wrongType)
            Text(
                "Choisissez une valeur du type attendu ou une formule de ce type.",
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
            )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            TextButton(onClick = { picking = true }) { Text("+ Insérer une variable") }
            if (resourceSource != null && vm != null)
                TextButton(onClick = { browsing = true }) { Text(resourceLabel) }
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
                        variables
                            .distinctBy { "${it.group}:${it.expression}" }
                            .filter {
                                "${it.label} ${it.expression} ${it.description}"
                                    .contains(search, true)
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
                                val fixedValue = held["value"]
                                val typedLiteral =
                                    mode == "literal" &&
                                        (fixedValue !is JsonPrimitive || !fixedValue.isString)
                                val typedSchema =
                                    schema != null &&
                                        schemaKind(schema, fixedValue ?: JsonNull) !in
                                            setOf("string", "json")
                                val typed =
                                    expects in setOf("number", "integer", "boolean") ||
                                        typedLiteral ||
                                        typedSchema
                                val replace = expects == "field" || (typed && mode != "formula")
                                val nextFormula =
                                    mode == "formula" ||
                                        expects in setOf("field", "formula") ||
                                        replace
                                val insertion =
                                    if (nextFormula) variable.expression
                                    else "{{${variable.expression}}}"
                                val selectedLiteral = literalSelection?.takeIf {
                                    runCatching { wireJson.parseToJsonElement(it.text) }
                                        .getOrNull() == fixedValue
                                }
                                val inputSelection = selectedLiteral ?: editorInput
                                val jsonString =
                                    mode == "literal" &&
                                        (expects == "json" ||
                                            (schema != null &&
                                                schemaKind(schema, fixedValue ?: JsonNull) ==
                                                    "json")) &&
                                        (fixedValue as? JsonPrimitive)?.isString == true
                                val content =
                                    if (jsonString) fixedValue!!.jsonPrimitive.content else text
                                val start =
                                    if (replace) 0
                                    else if (jsonString)
                                        jsonStringCaret(
                                            inputSelection.text,
                                            inputSelection.selection.min,
                                        )
                                    else inputSelection.selection.min
                                val end =
                                    if (replace) content.length
                                    else if (jsonString)
                                        jsonStringCaret(
                                            inputSelection.text,
                                            inputSelection.selection.max,
                                        )
                                    else inputSelection.selection.max
                                val nextText = content.take(start) + insertion + content.drop(end)
                                editorInput =
                                    TextFieldValue(nextText, TextRange(start + insertion.length))
                                change(
                                    if (nextFormula) formulaRuleValue(nextText)
                                    else templateValue(nextText)
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

private fun jsonStringCaret(encoded: String, at: Int): Int {
    val parsed = runCatching { wireJson.parseToJsonElement(encoded) }.getOrNull()
    if (parsed !is JsonPrimitive || !parsed.isString) return at
    for (end in minOf(at, encoded.length - 1) downTo 1) {
        val prefix = runCatching {
            wireJson.parseToJsonElement(encoded.take(end) + "\"").jsonPrimitive.content
        }
            .getOrNull()
        if (prefix != null) return prefix.length
    }
    return 0
}

private fun matchesRuleType(value: JsonElement?, type: String): Boolean =
    when (type) {
        "string" -> value is JsonPrimitive && value.isString
        "boolean" -> value is JsonPrimitive && !value.isString && value.booleanOrNull != null
        "integer" -> value is JsonPrimitive && !value.isString && value.longOrNull != null
        "number" ->
            value is JsonPrimitive && !value.isString && value.doubleOrNull?.isFinite() == true
        "array" -> value is JsonArray
        "object" -> value is JsonObject
        else -> true
    }

private fun fixedRuleValue(text: String, expects: String, schema: JsonObject?): JsonElement {
    val proposed =
        when (expects) {
            "number" ->
                text.toLongOrNull()?.let(::JsonPrimitive)
                    ?: JsonPrimitive(text.toDoubleOrNull()?.takeIf { it.isFinite() } ?: 0.0)
            "integer" -> JsonPrimitive(text.toLongOrNull() ?: 0L)
            "boolean" -> JsonPrimitive(text.trim() != "false")
            "json" ->
                runCatching { wireJson.parseToJsonElement(text) }.getOrElse { JsonPrimitive(text) }
            else -> JsonPrimitive(text)
        }
    return if (schema != null && !matchesRuleType(proposed, schemaKind(schema, proposed)))
        defaultRuleLiteral(schema)
    else proposed
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
            if (schema["properties"] == null && schema["items"] == null) "json"
            else if (schema["properties"] != null || value is JsonObject) "object"
            else if (schema["items"] != null || value is JsonArray) "array" else "string"
    }
}

@Composable
internal fun RuleLiteralEditor(
    schema: JsonObject,
    value: JsonElement,
    change: (JsonElement) -> Unit,
    depth: Int = 0,
    selectionChanged: ((TextFieldValue) -> Unit)? = null,
    jsonDrafts: JsonObject = JsonObject(emptyMap()),
    changeDraft: ((JsonElement, JsonObject) -> Unit)? = null,
) {
    fun publish(next: JsonElement, drafts: JsonObject = JsonObject(emptyMap())) {
        if (changeDraft == null) change(next) else changeDraft(next, drafts)
    }
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
                publish(options.first { it.toString() == key })
            }
        kind == "boolean" -> Toggle("Oui", value.flag() == true) { publish(JsonPrimitive(it)) }
        kind == "object" && !schema.obj("properties").isNullOrEmpty() && depth < 6 -> {
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
                        {
                            publish(
                                current.patch(name to it),
                                replaceChildJsonDrafts(jsonDrafts, name, JsonObject(emptyMap())),
                            )
                        },
                        depth + 1,
                        jsonDrafts = childJsonDrafts(jsonDrafts, name),
                        changeDraft = { next, drafts ->
                            publish(
                                current.patch(name to next),
                                replaceChildJsonDrafts(jsonDrafts, name, drafts),
                            )
                        },
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
                                    publish(
                                        JsonArray(
                                            values.toMutableList().apply { set(index, next) }
                                        ),
                                        replaceChildJsonDrafts(
                                            jsonDrafts,
                                            index,
                                            JsonObject(emptyMap()),
                                        ),
                                    )
                                },
                                depth + 1,
                                jsonDrafts = childJsonDrafts(jsonDrafts, index),
                                changeDraft = { next, drafts ->
                                    publish(
                                        JsonArray(
                                            values.toMutableList().apply { set(index, next) }
                                        ),
                                        replaceChildJsonDrafts(jsonDrafts, index, drafts),
                                    )
                                },
                            )
                            TextButton(
                                onClick = {
                                    publish(
                                        JsonArray(values.filterIndexed { at, _ -> at != index }),
                                        removeJsonDraftItem(jsonDrafts, index),
                                    )
                                }
                            ) {
                                Text("Retirer cet élément")
                            }
                        }
                    }
                }
                OutlinedButton(
                    onClick = {
                        publish(
                            JsonArray(values + defaultRuleLiteral(schema.obj("items").orEmpty())),
                            jsonDrafts,
                        )
                    }
                ) {
                    Text("+ Ajouter un élément")
                }
            }
        }
        kind in setOf("object", "array", "json") -> {
            fun matchesJson(candidate: JsonElement?): Boolean =
                candidate != null &&
                    (kind == "json" ||
                        kind == "array" && candidate is JsonArray ||
                        kind == "object" && candidate is JsonObject)
            fun display(value: JsonElement): String =
                (value as? JsonPrimitive)?.takeIf { it.isString && kind != "json" }?.content
                    ?: value.toString()
            val initialText =
                if (jsonDrafts.containsKey("")) jsonDrafts.obj("").text("text") else display(value)
            var draft by remember(kind) { mutableStateOf(TextFieldValue(initialText)) }
            var emitted by remember(kind) { mutableStateOf(value) }
            LaunchedEffect(value, jsonDrafts) {
                if (
                    value != emitted ||
                        jsonDrafts.containsKey("") && draft.text != jsonDrafts.obj("").text("text")
                ) {
                    draft = TextFieldValue(initialText)
                    emitted = value
                }
            }
            OutlinedTextField(
                draft,
                { input ->
                    val textChanged = input.text != draft.text
                    draft = input
                    selectionChanged?.invoke(input)
                    if (textChanged) {
                        val parsed = runCatching { parseRuleJsonInput(input.text) }
                        emitted =
                            parsed.getOrNull()?.takeIf { matchesJson(it) }
                                ?: JsonPrimitive(input.text)
                        publish(
                            emitted,
                            if (parsed.isFailure) jsonInputDraft(input.text)
                            else JsonObject(emptyMap()),
                        )
                    }
                },
                Modifier.fillMaxWidth(),
                label = {
                    Text(
                        when (kind) {
                            "array" -> "Liste JSON"
                            "object" -> "Objet JSON"
                            else -> "Valeur JSON"
                        }
                    )
                },
                minLines = 3,
                isError =
                    jsonDrafts.containsKey("") ||
                        runCatching { parseRuleJsonInput(draft.text) }.isFailure ||
                        !matchesJson(value),
                supportingText = {
                    Text(
                        when {
                            kind == "json" ->
                                "Texte entre guillemets, nombre, booléen, objet ou liste JSON."
                            kind == "array" && value is JsonArray -> "Valeurs de la liste en JSON."
                            kind == "array" -> "Saisissez une liste JSON valide."
                            value is JsonObject -> "Propriétés et valeurs de l’objet en JSON."
                            else -> "Saisissez un objet JSON valide."
                        }
                    )
                },
                shape = RoundedCornerShape(12.dp),
            )
        }
        kind in setOf("number", "integer") ->
            RuleNumericField(
                value,
                kind == "integer",
                "Valeur",
                hasProblem = jsonDrafts.isNotEmpty(),
                changeDraft = { next, drafts -> publish(next, drafts) },
            ) {
                publish(it)
            }
        else -> {
            val text = (value as? JsonPrimitive)?.contentOrNull.orEmpty()
            OutlinedTextField(
                text,
                { raw ->
                    publish(
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
    changeDraft: ((JsonElement, JsonObject) -> Unit)? = null,
    hasProblem: Boolean = false,
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
            val valid = (emitted as? JsonPrimitive)?.isString == false
            val drafts =
                if (valid) JsonObject(emptyMap())
                else
                    jsonInputDraft(
                        raw,
                        if (integer) "Saisissez un nombre entier." else "Saisissez un nombre.",
                    )
            if (changeDraft == null) change(emitted) else changeDraft(emitted, drafts)
        },
        Modifier.fillMaxWidth().semantics { contentDescription = label },
        singleLine = true,
        keyboardOptions =
            androidx.compose.foundation.text.KeyboardOptions(
                keyboardType = androidx.compose.ui.text.input.KeyboardType.Decimal
            ),
        isError =
            hasProblem ||
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
    key(vm.api.cacheScope, source) {
        ScopedRuleResourceSheet(
            source,
            { vm.api.pluginOperation(it, method = "GET") },
            choose,
            close,
        )
    }
}

@Composable
internal fun ScopedRuleResourceSheet(
    source: RuleResourceSource,
    request: suspend (String) -> JsonElement,
    choose: (JsonObject) -> Unit,
    close: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var page by remember { mutableStateOf<Long?>(1) }
    var items by remember { mutableStateOf<List<JsonObject>>(emptyList()) }
    var loading by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf("") }
    var generation by remember { mutableIntStateOf(0) }
    var paging by remember { mutableStateOf<Job?>(null) }
    val scope = rememberCoroutineScope()
    suspend fun load(next: Long = 1) {
        val asked = generation
        val askedQuery = query
        loading = true
        error = ""
        try {
            val path =
                "/plugins/${segment(source.pluginId)}/connections/${segment(source.instanceId)}/resources?resourceType=${segment(source.resourceType)}&agentId=${segment(source.agentId)}&query=${segment(askedQuery)}&page=$next"
            val result = request(path)
            if (asked != generation) return
            val found = result.list("items").map { it.jsonObject }
            items = if (next == 1L) found else (items + found).distinctBy { it.text("id") }
            page = result.jsonObject["nextPage"]?.jsonPrimitive?.longOrNull
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            if (asked == generation) error = e.message.orEmpty()
        } finally {
            if (asked == generation) loading = false
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
                {
                    if (query != it) {
                        generation++
                        paging?.cancel()
                        paging = null
                        query = it
                        items = emptyList()
                        page = null
                        loading = true
                        error = ""
                    }
                },
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
                            onClick = {
                                val next = page ?: return@TextButton
                                paging = scope.launch { load(next) }
                            },
                            enabled = !loading,
                        ) {
                            Text("Voir la suite")
                        }
                    }
            }
        }
    }
}
