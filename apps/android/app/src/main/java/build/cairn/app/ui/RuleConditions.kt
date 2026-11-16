@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.serialization.json.*

@Composable
internal fun RuleConditionEditor(
    value: JsonObject,
    variables: List<RuleVariable>,
    change: (JsonObject) -> Unit,
    vm: CairnViewModel,
    peopleSource: RuleResourceSource?,
    depth: Int = 0,
) {
    val group = value.text("type") in setOf("all", "any")
    val conditions = if (group) value.list("conditions") else listOf(value)
    val canAddCondition = if (group) depth + 2 <= 6 else depth + ruleConditionDepth(value) + 1 <= 6
    var adding by remember { mutableStateOf(false) }
    fun replace(index: Int, next: JsonObject) {
        if (!group) change(next)
        else
            change(
                value.patch(
                    "conditions" to JsonArray(conditions.toMutableList().apply { set(index, next) })
                )
            )
    }
    fun add(kind: String) {
        if (!canAddCondition) return
        val author = formulaRuleValue(authorVariable(variables))
        val left =
            formulaRuleValue(
                variables.firstOrNull { it.group == "Événement" }?.expression ?: "payload"
            )
        val condition =
            when (kind) {
                "all",
                "any" ->
                    ruleObject(
                        "type" to JsonPrimitive(kind),
                        "conditions" to JsonArray(emptyList()),
                    )
                "formula" ->
                    ruleObject("type" to JsonPrimitive(kind), "expression" to JsonPrimitive(""))
                "authorIn" ->
                    ruleObject(
                        "type" to JsonPrimitive(kind),
                        "author" to author,
                        "people" to JsonArray(emptyList()),
                    )
                "conversationExists" ->
                    ruleObject("type" to JsonPrimitive(kind), "key" to templateValue())
                "authorStartedConversation" ->
                    ruleObject(
                        "type" to JsonPrimitive(kind),
                        "author" to author,
                        "key" to templateValue(),
                    )
                "authorQuota" ->
                    ruleObject(
                        "type" to JsonPrimitive(kind),
                        "author" to author,
                        "limit" to JsonPrimitive(5),
                        "windowMinutes" to JsonPrimitive(60),
                    )
                else ->
                    ruleObject(
                        "type" to JsonPrimitive("compare"),
                        "left" to left,
                        "operator" to JsonPrimitive("equals"),
                        "right" to
                            typedComparisonRight(
                                templateValue(),
                                comparisonExpects(left, "equals", variables),
                                listOnly = false,
                            ),
                    )
            }
        change(
            ruleObject(
                "type" to JsonPrimitive(if (group) value.text("type") else "all"),
                "conditions" to JsonArray(conditions + condition),
            )
        )
        adding = false
    }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        if (group)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text("Continuer quand", modifier = Modifier.padding(top = 12.dp))
                FilterChip(
                    selected = value.text("type") == "all",
                    onClick = { change(value.patch("type" to JsonPrimitive("all"))) },
                    label = { Text("TOUTES") },
                )
                FilterChip(
                    selected = value.text("type") == "any",
                    onClick = { change(value.patch("type" to JsonPrimitive("any"))) },
                    label = { Text("AU MOINS UNE") },
                )
            }
        conditions.forEachIndexed { index, raw ->
            val condition = raw.jsonObject
            Surface(
                shape = RoundedCornerShape(16.dp),
                color = MaterialTheme.colorScheme.surfaceContainerLow,
            ) {
                Column(
                    Modifier.fillMaxWidth().padding(12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    when (condition.text("type")) {
                        "all",
                        "any" ->
                            RuleConditionEditor(
                                condition,
                                variables,
                                { replace(index, it) },
                                vm,
                                peopleSource,
                                depth + 1,
                            )
                        "not" -> {
                            Text(
                                "INVERSE : la condition ne doit pas être remplie",
                                style = MaterialTheme.typography.labelMedium,
                            )
                            RuleConditionEditor(
                                condition.obj("condition").orEmpty(),
                                variables,
                                { replace(index, condition.patch("condition" to it)) },
                                vm,
                                peopleSource,
                                depth + if (group) 2 else 1,
                            )
                        }
                        "compare" -> {
                            RuleValueEditor(
                                "Champ à vérifier",
                                condition["left"],
                                variables,
                                { left ->
                                    replace(
                                        index,
                                        condition.patch(
                                            "left" to left,
                                            "right" to
                                                if (binaryOperator(condition.text("operator")))
                                                    typedComparisonRight(
                                                        condition["right"],
                                                        comparisonExpects(
                                                            left,
                                                            condition.text("operator"),
                                                            variables,
                                                        ),
                                                        listOnly =
                                                            condition.text("operator") == "in",
                                                    )
                                                else null,
                                        ),
                                    )
                                },
                                expects = "field",
                            )
                            Choice(
                                "Test",
                                condition.text("operator"),
                                ruleOperatorTitles.toList(),
                            ) { operator ->
                                replace(
                                    index,
                                    condition.patch(
                                        "operator" to JsonPrimitive(operator),
                                        "right" to
                                            if (binaryOperator(operator))
                                                typedComparisonRight(
                                                    condition["right"],
                                                    comparisonExpects(
                                                        condition["left"],
                                                        operator,
                                                        variables,
                                                    ),
                                                    listOnly = operator == "in",
                                                )
                                            else null,
                                    ),
                                )
                            }
                            if (binaryOperator(condition.text("operator")))
                                RuleValueEditor(
                                    "Comparer avec",
                                    condition["right"],
                                    variables,
                                    { replace(index, condition.patch("right" to it)) },
                                    expects =
                                        comparisonExpects(
                                            condition["left"],
                                            condition.text("operator"),
                                            variables,
                                        ),
                                    schema =
                                        if (condition.text("operator") == "in")
                                            comparisonListSchema(condition["left"], variables)
                                        else null,
                                )
                        }
                        "formula" ->
                            RuleValueEditor(
                                "Formule vraie ou fausse",
                                formulaRuleValue(condition.text("expression")),
                                variables,
                                {
                                    replace(
                                        index,
                                        condition.patch(
                                            "expression" to JsonPrimitive(it.text("expression"))
                                        ),
                                    )
                                },
                                expects = "formula",
                                multiline = true,
                            )
                        "authorIn" -> {
                            Text(
                                "L’auteur fait partie de…",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            RuleValueEditor(
                                "Auteur de l’événement",
                                condition["author"],
                                variables,
                                { replace(index, condition.patch("author" to it)) },
                                expects = "field",
                            )
                            RulePeopleChoices(vm, condition.list("people"), peopleSource) { people
                                ->
                                replace(index, condition.patch("people" to JsonArray(people)))
                            }
                        }
                        "conversationExists",
                        "authorStartedConversation" -> {
                            Text(
                                if (condition.text("type") == "conversationExists")
                                    "Une conversation existe pour cette clé"
                                else "L’auteur a démarré la conversation de cette clé",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            RuleValueEditor(
                                "Clé de conversation",
                                condition["key"],
                                variables,
                                { replace(index, condition.patch("key" to it)) },
                            )
                            if (condition.text("type") == "authorStartedConversation")
                                RuleValueEditor(
                                    "Auteur",
                                    condition["author"],
                                    variables,
                                    { replace(index, condition.patch("author" to it)) },
                                    expects = "field",
                                )
                        }
                        "authorQuota" -> {
                            Text(
                                "Limiter le nombre d’événements par auteur",
                                style = MaterialTheme.typography.titleSmall,
                            )
                            RuleValueEditor(
                                "Auteur",
                                condition["author"],
                                variables,
                                { replace(index, condition.patch("author" to it)) },
                                expects = "field",
                            )
                            Field(
                                "Au plus",
                                condition.text("limit"),
                                {
                                    replace(
                                        index,
                                        condition.patch(
                                            "limit" to
                                                (it.toLongOrNull()?.let(::JsonPrimitive)
                                                    ?: JsonPrimitive(it))
                                        ),
                                    )
                                },
                            )
                            Field(
                                "Pendant une période de (minutes)",
                                condition.text("windowMinutes"),
                                {
                                    replace(
                                        index,
                                        condition.patch(
                                            "windowMinutes" to
                                                (it.toLongOrNull()?.let(::JsonPrimitive)
                                                    ?: JsonPrimitive(it))
                                        ),
                                    )
                                },
                            )
                        }
                    }
                    Row {
                        if (condition.text("type") != "not")
                            TextButton(
                                enabled =
                                    depth + (if (group) 2 else 1) + ruleConditionDepth(condition) <=
                                        6,
                                onClick = {
                                    replace(
                                        index,
                                        ruleObject(
                                            "type" to JsonPrimitive("not"),
                                            "condition" to condition,
                                        ),
                                    )
                                },
                            ) {
                                Text("Inverser")
                            }
                        else
                            TextButton(
                                onClick = { replace(index, condition.obj("condition").orEmpty()) }
                            ) {
                                Text("Ne plus inverser")
                            }
                        Spacer(Modifier.weight(1f))
                        TextButton(
                            onClick = {
                                change(
                                    ruleObject(
                                        "type" to
                                            JsonPrimitive(if (group) value.text("type") else "all"),
                                        "conditions" to
                                            JsonArray(
                                                conditions.filterIndexed { at, _ -> at != index }
                                            ),
                                    )
                                )
                            }
                        ) {
                            Text("Retirer")
                        }
                    }
                }
            }
        }
        if (conditions.isEmpty())
            Text(
                "Ajoutez une condition à ce groupe.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        Box {
            OutlinedButton(onClick = { adding = true }, enabled = canAddCondition) {
                Text("+ Ajouter une condition")
            }
            DropdownMenu(adding, { adding = false }) {
                val choices =
                    listOf(
                        "compare" to "Comparer un champ",
                        "authorIn" to "Choisir des personnes",
                        "conversationExists" to "Conversation existante",
                        "authorStartedConversation" to "Auteur de la conversation",
                        "authorQuota" to "Quota par auteur",
                        "formula" to "Formule CEL",
                    ) +
                        // The server counts the root at level 1; a new group must still fit its
                        // leaf.
                        if (canAddCondition && depth + 3 <= 6)
                            listOf("all" to "Groupe ET", "any" to "Groupe OU")
                        else emptyList()
                choices.forEach { (kind, title) ->
                    DropdownMenuItem(text = { Text(title) }, onClick = { add(kind) })
                }
            }
        }
    }
}

@Composable
private fun RulePeopleChoices(
    vm: CairnViewModel,
    people: List<JsonElement>,
    source: RuleResourceSource?,
    changed: (List<JsonElement>) -> Unit,
) {
    var browsing by remember { mutableStateOf(false) }
    var manual by remember { mutableStateOf(false) }
    var draft by remember { mutableStateOf("") }
    var names by
        remember(vm.api.cacheScope, source) { mutableStateOf<Map<String, String>>(emptyMap()) }
    LaunchedEffect(people, source, vm.api.cacheScope) {
        if (source != null)
            for (person in people) {
                val id = person.text("id")
                if (id !in names)
                    try {
                        val resource =
                            vm.api
                                .pluginOperation(
                                    "/plugins/${segment(source.pluginId)}/connections/${segment(source.instanceId)}/resources/ancestors?resourceType=${segment(person.text("resourceType"))}&agentId=${segment(source.agentId)}&id=${segment(id)}",
                                    method = "GET",
                                )
                                .obj("resource")
                        if (resource != null) names = names + (id to resource.text("name"))
                    } catch (e: Exception) {
                        if (e is CancellationException) throw e
                    }
            }
    }
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        people.forEach { person ->
            InputChip(
                selected = true,
                onClick = { changed(people.filter { it != person }) },
                label = { Text(names[person.text("id")] ?: "Personne sélectionnée") },
                trailingIcon = { Text("×") },
            )
        }
    }
    if (source != null)
        OutlinedButton(onClick = { browsing = true }) { Text("Choisir une personne…") }
    TextButton(onClick = { manual = !manual }) { Text("Identifiant précis (avancé)") }
    if (manual) {
        Field("Identifiant", draft, { draft = it })
        TextButton(
            onClick = {
                val id = draft.trim()
                if (id.isNotBlank() && people.none { it.text("id") == id })
                    changed(
                        people +
                            ruleObject(
                                "resourceType" to JsonPrimitive(source?.resourceType ?: "person"),
                                "id" to JsonPrimitive(id),
                            )
                    )
                draft = ""
            }
        ) {
            Text("Ajouter")
        }
    }
    if (browsing && source != null)
        RuleResourceSheet(
            vm,
            source,
            { resource ->
                val id = resource.text("id")
                names = names + (id to resource.text("name"))
                if (people.none { it.text("id") == id })
                    changed(
                        people +
                            ruleObject(
                                "resourceType" to JsonPrimitive(resource.text("resourceType")),
                                "id" to JsonPrimitive(id),
                            )
                    )
                browsing = false
            },
            { browsing = false },
        )
}
