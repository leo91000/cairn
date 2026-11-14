@file:OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)

package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.HookRuleCatalog
import kotlinx.serialization.json.*

@Composable
internal fun RuleStepPanel(
    vm: CairnViewModel,
    state: Workspace,
    node: JsonObject,
    catalog: HookRuleCatalog,
    executionAgent: String,
    variables: List<RuleVariable>,
    peopleSource: RuleResourceSource?,
    changed: (JsonObject) -> Unit,
) {
    val kind = node.text("type")
    val action = node.obj("action").orEmpty()
    var advanced by remember { mutableStateOf(false) }
    fun patchAction(vararg values: Pair<String, JsonElement?>) =
        changed(node.patch("action" to action.patch(*values)))
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Text(
            if (kind == "action") ruleStepChoice(action.text("type")).title
            else ruleStepChoice(kind).title,
            style = MaterialTheme.typography.headlineSmall,
        )
        if (kind == "action")
            Field(
                "Nom affiché (facultatif)",
                node.text("label"),
                { changed(node.patch("label" to JsonPrimitive(it))) },
            )
        when (kind) {
            "if" ->
                RuleConditionEditor(
                    node.obj("condition").orEmpty(),
                    variables,
                    { changed(node.patch("condition" to it)) },
                    vm,
                    peopleSource,
                )
            "forEach" -> {
                RuleValueEditor(
                    "Liste à parcourir",
                    node["items"],
                    variables,
                    { changed(node.patch("items" to it)) },
                    expects = "field",
                )
                Field(
                    "Nom de chaque élément",
                    node.text("itemName"),
                    { changed(node.patch("itemName" to JsonPrimitive(it))) },
                )
                Text(
                    "Les étapes du chemin recevront cet élément comme variable.",
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            "action" ->
                when (action.text("type")) {
                    "callTool" -> {
                        val tools = catalog.tools.filter { executionAgent in it.agentIds }
                        val tool = tools.find {
                            it.instanceId == action.text("instanceId") &&
                                it.tool == action.text("tool")
                        }
                        Choice(
                            "Outil",
                            "${action.text("instanceId")}:${action.text("tool")}",
                            tools.map {
                                "${it.instanceId}:${it.tool}" to "${it.instanceName} · ${it.tool}"
                            },
                        ) { key ->
                            val selected = tools.first { "${it.instanceId}:${it.tool}" == key }
                            val schema =
                                selected.inputSchema as? JsonObject ?: JsonObject(emptyMap())
                            val required =
                                schema.list("required").map { it.jsonPrimitive.content }.toSet()
                            val inputs =
                                schema
                                    .obj("properties")
                                    .orEmpty()
                                    .filterKeys { it in required }
                                    .mapValues { (_, value) ->
                                        defaultRuleInput(
                                            resolveRuleSchema(
                                                value as? JsonObject ?: JsonObject(emptyMap()),
                                                schema,
                                            )
                                        )
                                    }
                            patchAction(
                                "instanceId" to JsonPrimitive(selected.instanceId),
                                "tool" to JsonPrimitive(selected.tool),
                                "input" to JsonObject(inputs),
                            )
                        }
                        if (tools.isEmpty())
                            Text(
                                "Cet agent n’a pas d’outil de plugin disponible. Choisissez un autre agent ou ajustez ses droits.",
                                color = MaterialTheme.colorScheme.error,
                            )
                        if (tool != null) {
                            if (tool.description.isNotBlank())
                                Text(tool.description, style = MaterialTheme.typography.bodySmall)
                            val schema = tool.inputSchema as? JsonObject ?: JsonObject(emptyMap())
                            val fields = schema.obj("properties").orEmpty()
                            val required =
                                schema.list("required").map { it.jsonPrimitive.content }.toSet()
                            val input = action.obj("input").orEmpty()
                            fields.forEach { (name, rawSchema) ->
                                val fieldSchema =
                                    resolveRuleSchema(
                                        rawSchema as? JsonObject ?: JsonObject(emptyMap()),
                                        schema,
                                    )
                                if (name in required || name in input) {
                                    val field =
                                        tool.resourceFields.find {
                                            it.path ==
                                                "/${name.replace("~", "~0").replace("/", "~1")}"
                                        }
                                    RuleValueEditor(
                                        fieldSchema.text("title").ifBlank { name },
                                        input[name] ?: defaultRuleInput(fieldSchema),
                                        variables,
                                        { patchAction("input" to input.patch(name to it)) },
                                        expects =
                                            when (fieldSchema.text("type")) {
                                                "number",
                                                "integer" -> "number"
                                                "boolean" -> "boolean"
                                                "object",
                                                "array" -> "json"
                                                else -> "text"
                                            },
                                        schema = fieldSchema,
                                        resourceSource =
                                            field?.let {
                                                RuleResourceSource(
                                                    tool.pluginId,
                                                    tool.instanceId,
                                                    it.resourceType,
                                                )
                                            },
                                        vm = vm,
                                    )
                                    if (fieldSchema.text("description").isNotBlank())
                                        Text(
                                            fieldSchema.text("description"),
                                            style = MaterialTheme.typography.bodySmall,
                                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                                        )
                                    if (name !in required)
                                        TextButton(
                                            onClick = {
                                                patchAction("input" to input.patch(name to null))
                                            }
                                        ) {
                                            Text("Retirer ce champ")
                                        }
                                } else
                                    TextButton(
                                        onClick = {
                                            patchAction(
                                                "input" to
                                                    input.patch(
                                                        name to defaultRuleInput(fieldSchema)
                                                    )
                                            )
                                        }
                                    ) {
                                        Text("+ ${fieldSchema.text("title").ifBlank { name }}")
                                    }
                            }
                        }
                    }
                    "startAgent" -> {
                        Choice(
                            "Agent à démarrer",
                            action.text("agentId"),
                            catalog.agents.map { it.agentId to it.name },
                        ) {
                            patchAction("agentId" to JsonPrimitive(it))
                        }
                        RuleValueEditor(
                            "Message à l’agent",
                            action["prompt"],
                            variables,
                            { patchAction("prompt" to it) },
                            multiline = true,
                        )
                        RuleValueEditor(
                            "Clé de conversation (facultative)",
                            action["key"],
                            variables,
                            {
                                patchAction(
                                    "key" to
                                        if (
                                            it.text("type") == "template" &&
                                                it.text("text").isBlank()
                                        )
                                            null
                                        else it
                                )
                            },
                        )
                        Choice(
                            "Projet",
                            action.text("projectId"),
                            listOf("" to "Sans projet") + state.projects.map { it.id to it.name },
                        ) {
                            patchAction(
                                "projectId" to it.takeIf(String::isNotBlank)?.let(::JsonPrimitive)
                            )
                        }
                    }
                    "steerConversation",
                    "answerQuestion",
                    "cancelConversation" -> {
                        RuleValueEditor(
                            "Clé de conversation",
                            action["key"],
                            variables,
                            { patchAction("key" to it) },
                        )
                        when (action.text("type")) {
                            "steerConversation" ->
                                RuleValueEditor(
                                    "Message à ajouter",
                                    action["message"],
                                    variables,
                                    { patchAction("message" to it) },
                                    multiline = true,
                                )
                            "answerQuestion" ->
                                RuleValueEditor(
                                    "Réponse à la question",
                                    action["answer"],
                                    variables,
                                    { patchAction("answer" to it) },
                                    multiline = true,
                                )
                        }
                    }
                    "storeValue" -> {
                        Choice(
                            "Mémoriser pour",
                            action.text("scope"),
                            listOf(
                                "rule" to "Cette règle",
                                "conversation" to "La conversation d’une clé",
                            ),
                        ) {
                            patchAction("scope" to JsonPrimitive(it))
                        }
                        if (action.text("scope") == "conversation")
                            RuleValueEditor(
                                "Clé de conversation",
                                action["key"],
                                variables,
                                { patchAction("key" to it) },
                            )
                        Field(
                            "Nom de la valeur",
                            action.text("name"),
                            { patchAction("name" to JsonPrimitive(it)) },
                        )
                        RuleValueEditor(
                            "Valeur à mémoriser",
                            action["value"],
                            variables,
                            { patchAction("value" to it) },
                        )
                    }
                    "wait" -> {
                        RuleValueEditor(
                            "Durée de l’attente (secondes)",
                            action["seconds"],
                            variables,
                            { patchAction("seconds" to it) },
                            expects = "number",
                        )
                        Text(
                            "Entre une seconde et sept jours. Le déclenchement reprend ensuite à l’étape suivante.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
        }
        HorizontalDivider()
        TextButton(onClick = { advanced = !advanced }) {
            Text(if (advanced) "Références avancées ⌃" else "Références avancées ⌄")
        }
        if (advanced) {
            Field(
                "Identifiant de l’étape",
                node.text("id"),
                { changed(node.patch("id" to JsonPrimitive(it))) },
            )
            Text(
                "Les étapes suivantes utilisent steps.${node.text("id")}. Un renommage peut demander d’ajuster leurs formules.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}
