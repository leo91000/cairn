package build.cairn.app.data

import build.cairn.contracts.HookRuleCatalog
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

// Hook rules (ADR-0075): what the rules screens say about a rule and how the editor changes its
// tree. The editor keeps the definition as JSON, the shape the installation validates: nested
// blocks are not typed in the generated contracts. The installation checks everything again.

/** A kind of step the editor adds. */
data class RuleStepChoice(val kind: String, val title: String, val description: String)

val ruleStepChoices =
    listOf(
        RuleStepChoice(
            "callTool",
            "Appeler un outil",
            "Publier, réagir, lire… avec les droits de l’agent.",
        ),
        RuleStepChoice(
            "startAgent",
            "Démarrer un agent",
            "Ouvrir une conversation, liée ou non à une clé.",
        ),
        RuleStepChoice(
            "steerConversation",
            "Orienter une conversation",
            "Envoyer un message à la conversation d’une clé.",
        ),
        RuleStepChoice(
            "answerQuestion",
            "Répondre à une question",
            "Répondre à la question en attente de l’agent.",
        ),
        RuleStepChoice(
            "cancelConversation",
            "Arrêter une conversation",
            "Arrêter le tour en cours d’une conversation.",
        ),
        RuleStepChoice(
            "storeValue",
            "Mémoriser une valeur",
            "Garder une valeur pour les déclenchements suivants.",
        ),
        RuleStepChoice("wait", "Attendre", "Faire une pause, puis continuer (7 jours au plus)."),
        RuleStepChoice("if", "Si / sinon", "Choisir entre deux chemins."),
        RuleStepChoice(
            "forEach",
            "Pour chaque",
            "Répéter des étapes pour chaque élément d’une liste.",
        ),
    )

fun ruleStepChoice(kind: String) =
    ruleStepChoices.firstOrNull { it.kind == kind } ?: ruleStepChoices.first()

val cairnHookTitles =
    mapOf(
        "replyReady" to "Réponse prête",
        "questionAsked" to "Question posée",
        "progress" to "Progression",
        "ended" to "Tour terminé",
    )

val ruleBlockTitles =
    mapOf(
        "disabled" to "Désactivée",
        "emergencyPaused" to "En pause d’urgence",
        "budgetExhausted" to "Limite horaire atteinte",
        "rightsLost" to "Droits perdus",
        "membershipEnded" to "Créateur parti",
    )

val ruleBlockLifters =
    mapOf(
        "disabled" to "Son créateur peut la réactiver.",
        "emergencyPaused" to "Seul le propriétaire peut la relancer.",
        "budgetExhausted" to "Son créateur ou le propriétaire peut la relancer.",
        "rightsLost" to
            "Elle reprend d’elle-même quand le droit revient, ou quand son créateur la modifie.",
        "membershipEnded" to "Elle ne reprend jamais : dupliquez-la pour l’utiliser.",
    )

val triggeringStatusTitles =
    mapOf(
        "running" to "En cours",
        "waiting" to "En attente",
        "retrying" to "Nouvel essai",
        "completed" to "Terminé",
        "failed" to "Échec",
        "stopped" to "Arrêté",
    )

val ruleOperatorTitles =
    linkedMapOf(
        "equals" to "est",
        "notEquals" to "n’est pas",
        "contains" to "contient",
        "startsWith" to "commence par",
        "endsWith" to "finit par",
        "matches" to "correspond à",
        "in" to "est l’un de",
        "present" to "est présent",
        "absent" to "est absent",
        "greaterThan" to "est supérieur à",
        "lessThan" to "est inférieur à",
    )

fun binaryOperator(operator: String) = operator != "present" && operator != "absent"

internal fun JsonElement?.text(key: String): String =
    ((this as? JsonObject)?.get(key) as? JsonPrimitive)?.contentOrNull.orEmpty()

internal fun JsonElement?.obj(key: String): JsonObject? =
    (this as? JsonObject)?.get(key) as? JsonObject

internal fun JsonElement?.list(key: String): List<JsonElement> =
    ((this as? JsonObject)?.get(key) as? JsonArray).orEmpty()

fun triggerKey(trigger: JsonElement?): String =
    if (trigger.text("type") == "plugin")
        "plugin:${trigger.text("instanceId")}:${trigger.text("hook")}"
    else "cairn:${trigger.text("hook")}"

/** Where the event comes from and what it is, for a trigger. */
fun describeTrigger(trigger: JsonElement?, catalog: HookRuleCatalog?): Pair<String, String> {
    if (trigger.text("type") == "cairn")
        return "Cairn" to (cairnHookTitles[trigger.text("hook")] ?: trigger.text("hook"))
    val hook =
        catalog?.hooks?.firstOrNull {
            triggerKey(wireJson.encodeToJsonElement(it.trigger)) == triggerKey(trigger)
        }
    return (hook?.instanceName ?: "Plugin") to trigger.text("hook")
}

/** A value on one line: fixed text, text with insertions, or a formula. */
fun describeValue(value: JsonElement?): String {
    if (value !is JsonObject) return ""
    return when (value.text("type")) {
        "literal" ->
            when (val literal = value["value"]) {
                is JsonPrimitive -> literal.contentOrNull ?: "null"
                null -> ""
                else -> literal.toString()
            }
        "template" -> value.text("text")
        else -> "ƒ ${value.text("expression")}"
    }
}

private fun quoted(value: JsonElement?): String {
    val text = describeValue(value)
    val literal =
        value.text("type") == "literal" &&
            (value as? JsonObject)?.get("value")?.let { it is JsonPrimitive && it.isString } == true
    return if (literal) "« $text »" else text
}

fun describeCondition(condition: JsonElement?): String =
    when (condition.text("type")) {
        "all",
        "any" -> {
            val parts =
                condition.list("conditions").map(::describeCondition).filter { it.isNotBlank() }
            when {
                parts.isEmpty() -> "Ajouter une condition"
                parts.size == 1 -> parts.first()
                else ->
                    parts.joinToString(if (condition.text("type") == "all") " et " else " ou ") {
                        if (" et " in it || " ou " in it) "($it)" else it
                    }
            }
        }
        "not" -> "non (${describeCondition(condition.obj("condition"))})"
        "compare" -> {
            val operator = condition.text("operator")
            listOf(
                    describeValue(condition.obj("left")),
                    ruleOperatorTitles[operator] ?: operator,
                    if (binaryOperator(operator)) quoted(condition.obj("right")) else "",
                )
                .filter { it.isNotBlank() }
                .joinToString(" ")
        }
        "formula" -> "ƒ ${condition.text("expression")}"
        "authorIn" -> {
            val count = condition.list("people").size
            "${describeValue(condition.obj("author"))} est l’une des $count personne${if (count > 1) "s" else ""} choisie${if (count > 1) "s" else ""}"
        }
        "authorStartedConversation" ->
            "${describeValue(condition.obj("author"))} a démarré ${quoted(condition.obj("key"))}"
        "conversationExists" -> "une conversation existe pour ${quoted(condition.obj("key"))}"
        "authorQuota" ->
            "${describeValue(condition.obj("author"))} a déclenché moins de ${condition.text("limit")} fois"
        else -> ""
    }

fun ruleDuration(seconds: Long): String =
    when {
        seconds % 86_400 == 0L -> "${seconds / 86_400} j"
        seconds % 3_600 == 0L -> "${seconds / 3_600} h"
        seconds % 60 == 0L -> "${seconds / 60} min"
        else -> "$seconds s"
    }

/** A title and a detail line for an action. */
fun describeAction(
    action: JsonElement?,
    catalog: HookRuleCatalog?,
    agentName: (String) -> String,
): Pair<String, String> =
    when (action.text("type")) {
        "callTool" -> {
            val tool =
                catalog?.tools?.firstOrNull {
                    it.instanceId == action.text("instanceId") && it.tool == action.text("tool")
                }
            val input =
                action.obj("input").orEmpty().entries.take(2).joinToString(" · ") { (key, value) ->
                    "$key : ${describeValue(value)}"
                }
            (tool?.let { "${it.instanceName} · ${it.tool}" }
                ?: action.text("tool").ifBlank { "Outil" }) to input
        }
        "startAgent" ->
            "Démarrer ${agentName(action.text("agentId"))}" to
                listOf(
                        describeValue(action.obj("prompt")),
                        action.obj("author")?.let { "auteur ${describeValue(it)}" }.orEmpty(),
                        action.obj("key")?.let { "clé ${describeValue(it)}" }.orEmpty(),
                    )
                    .filter { it.isNotBlank() }
                    .joinToString(" · ")
        "steerConversation" ->
            "Orienter la conversation" to
                "${describeValue(action.obj("key"))} · ${describeValue(action.obj("message"))}"
        "answerQuestion" ->
            "Répondre à sa question" to
                "${describeValue(action.obj("key"))} · ${describeValue(action.obj("answer"))}"
        "cancelConversation" -> "Arrêter la conversation" to describeValue(action.obj("key"))
        "storeValue" -> "Mémoriser ${action.text("name")}" to describeValue(action.obj("value"))
        "wait" -> {
            val seconds = action.obj("seconds")?.get("value")?.jsonPrimitive?.longOrNull
            "Attendre ${seconds?.let(::ruleDuration) ?: describeValue(action.obj("seconds"))}" to
                "Puis continuer avec les étapes suivantes."
        }
        else -> action.text("type") to ""
    }

// ——— Tree editing, by JSON Pointer into the definition, as validation reports problems.

private fun segments(path: String) = path.split('/').filter { it.isNotEmpty() }

fun JsonElement.at(path: String): JsonElement? {
    var current: JsonElement? = this
    for (part in segments(path)) {
        current =
            when (current) {
                is JsonObject -> current[part]
                is JsonArray -> part.toIntOrNull()?.let { current.getOrNull(it) }
                else -> null
            }
    }
    return current
}

/** The element with the value at [path] replaced, or removed when [value] is null. */
fun JsonElement.with(path: String, value: JsonElement?): JsonElement {
    val parts = segments(path)
    if (parts.isEmpty()) return value ?: JsonNull
    val head = parts.first()
    val rest = "/" + parts.drop(1).joinToString("/")
    return when (this) {
        is JsonObject -> {
            val child = this[head]
            val next =
                if (parts.size == 1) value else (child ?: JsonObject(emptyMap())).with(rest, value)
            JsonObject(if (next == null) this - head else this + (head to next))
        }
        is JsonArray -> {
            val index = head.toIntOrNull() ?: return this
            val items = toMutableList()
            if (parts.size == 1) {
                if (value == null) items.removeAt(index) else items[index] = value
            } else items[index] = items[index].with(rest, value)
            JsonArray(items)
        }
        else -> this
    }
}

fun JsonElement.inserted(container: String, index: Int, node: JsonElement): JsonElement {
    val items = (at(container) as? JsonArray).orEmpty().toMutableList()
    items.add(index.coerceIn(0, items.size), node)
    return with(container, JsonArray(items))
}

fun JsonElement.moved(path: String, offset: Int): JsonElement {
    val container = path.substringBeforeLast('/')
    val from = path.substringAfterLast('/').toIntOrNull() ?: return this
    val items = (at(container) as? JsonArray).orEmpty().toMutableList()
    val to = from + offset
    if (to !in items.indices) return this
    val node = items.removeAt(from)
    items.add(to, node)
    return with(container, JsonArray(items))
}

private fun uniqueLoopItemName(definition: JsonElement): String {
    val taken =
        allNodes(definition.list("steps"))
            .filter { it.text("type") == "forEach" }
            .map { it.text("itemName") }
            .toSet()
    var name = "item"
    var index = 2
    while (name in taken) name = "item${index++}"
    return name
}

private fun allNodes(nodes: List<JsonElement>): List<JsonElement> = nodes.flatMap { node ->
    listOf(node) +
        when (node.text("type")) {
            "if" -> allNodes(node.list("then")) + allNodes(node.list("otherwise"))
            "forEach" -> allNodes(node.list("body"))
            else -> emptyList()
        }
}

fun uniqueStepId(definition: JsonElement, base: String): String {
    val taken = allNodes(definition.list("steps")).map { it.text("id") }.toSet()
    if (base !in taken) return base
    return generateSequence(2) { it + 1 }.map { "$base$it" }.first { it !in taken }
}

private val idBases =
    mapOf(
        "callTool" to "call",
        "startAgent" to "start",
        "steerConversation" to "steer",
        "answerQuestion" to "answer",
        "cancelConversation" to "stop",
        "storeValue" to "remember",
        "wait" to "wait",
        "if" to "check",
        "forEach" to "each",
    )

fun templateValue(text: String = ""): JsonObject = buildJsonObject {
    put("type", "template")
    put("text", text)
}

fun optionalRuleText(value: JsonObject): JsonObject? {
    if (value.text("type") == "template" && value.text("text").isEmpty()) return null
    if (value.text("type") == "literal") {
        val literal = value["value"]
        if (
            literal == JsonNull ||
                literal is JsonPrimitive && literal.isString && literal.content.isEmpty()
        )
            return null
    }
    return value
}

fun newRuleStep(definition: JsonElement, kind: String, catalog: HookRuleCatalog?): JsonObject {
    val id = uniqueStepId(definition, idBases[kind] ?: "step")
    val agent = definition.text("executionAgentId")
    fun action(content: JsonObject) = buildJsonObject {
        put("type", "action")
        put("id", id)
        put("action", content)
    }
    return when (kind) {
        "if" ->
            buildJsonObject {
                put("type", "if")
                put("id", id)
                put(
                    "condition",
                    buildJsonObject {
                        put("type", "all")
                        put("conditions", JsonArray(emptyList()))
                    },
                )
                put("then", JsonArray(emptyList()))
                put("otherwise", JsonArray(emptyList()))
            }
        "forEach" ->
            buildJsonObject {
                put("type", "forEach")
                put("id", id)
                put(
                    "items",
                    buildJsonObject {
                        put("type", "formula")
                        put("expression", "payload.")
                    },
                )
                put("itemName", uniqueLoopItemName(definition))
                put("body", JsonArray(emptyList()))
            }
        "callTool" -> {
            val tool =
                catalog?.tools?.firstOrNull { agent in it.agentIds }
                    ?: catalog?.tools?.firstOrNull()
            action(
                buildJsonObject {
                    put("type", "callTool")
                    put("instanceId", tool?.instanceId.orEmpty())
                    put("tool", tool?.tool.orEmpty())
                    val schema =
                        ruleToolInputSchema(
                            tool?.inputSchema as? JsonObject ?: JsonObject(emptyMap())
                        )
                    val required = schema.list("required").map { it.jsonPrimitive.content }.toSet()
                    put(
                        "input",
                        JsonObject(
                            schema
                                .obj("properties")
                                .orEmpty()
                                .filterKeys { it in required }
                                .mapValues { (_, field) ->
                                    defaultRuleInput(
                                        resolveRuleSchema(
                                            field as? JsonObject ?: JsonObject(emptyMap()),
                                            schema,
                                        )
                                    )
                                }
                        ),
                    )
                }
            )
        }
        "startAgent" ->
            action(
                buildJsonObject {
                    put("type", "startAgent")
                    put("agentId", agent)
                    put("prompt", templateValue())
                }
            )
        "steerConversation" ->
            action(
                buildJsonObject {
                    put("type", "steerConversation")
                    put("key", templateValue())
                    put("message", templateValue())
                }
            )
        "answerQuestion" ->
            action(
                buildJsonObject {
                    put("type", "answerQuestion")
                    put("key", templateValue())
                    put("answer", templateValue())
                }
            )
        "cancelConversation" ->
            action(
                buildJsonObject {
                    put("type", "cancelConversation")
                    put("key", templateValue())
                }
            )
        "storeValue" ->
            action(
                buildJsonObject {
                    put("type", "storeValue")
                    put("scope", "rule")
                    put("name", "last")
                    put("value", templateValue())
                }
            )
        else ->
            action(
                buildJsonObject {
                    put("type", "wait")
                    put(
                        "seconds",
                        buildJsonObject {
                            put("type", "literal")
                            put("value", 600)
                        },
                    )
                }
            )
    }
}

/** Problems of a node, its fields and its nested blocks. */
fun problemsUnder(problems: List<build.cairn.contracts.HookRuleProblem>, path: String) =
    problems.filter {
        it.path == path || it.path.startsWith("$path/")
    }

// ——— Variables a value can insert at a place of the tree.

data class RuleVariable(
    val group: String,
    val expression: String,
    val label: String,
    val type: String,
    val description: String,
    val example: String? = null,
    val path: String? = null,
    val itemType: String? = null,
    val schema: JsonObject? = null,
    val personId: Boolean = false,
)

private fun schemaType(schema: JsonElement?): String {
    val type = (schema as? JsonObject)?.get("type")
    return when (type) {
        is JsonArray ->
            type
                .firstOrNull { it.jsonPrimitive.contentOrNull != "null" }
                ?.jsonPrimitive
                ?.contentOrNull ?: "valeur"
        is JsonPrimitive -> type.contentOrNull ?: "valeur"
        else -> "valeur"
    }
}

fun schemaFields(
    schema: JsonElement?,
    prefix: String,
    depth: Int = 0,
    pointer: String = "",
): List<RuleVariable> {
    val resolved =
        if (depth == 0) resolveRuleSchema(schema as? JsonObject ?: JsonObject(emptyMap()))
        else schema
    val properties = resolved.obj("properties") ?: return emptyList()
    if (depth > 3) return emptyList()
    return properties.flatMap { (name, field) ->
        val expression =
            if (Regex("""^[A-Za-z_]\w*$""").matches(name)) "$prefix.$name"
            else "$prefix[${JsonPrimitive(name)}]"
        val path = "$pointer/${name.replace("~", "~0").replace("/", "~1")}"
        listOf(
            RuleVariable(
                "Événement",
                expression,
                expression.removePrefix("payload."),
                schemaType(field),
                field.text("description"),
                path = path,
                itemType = schemaType(field.obj("items")),
                schema = field as? JsonObject,
            )
        ) + schemaFields(field, expression, depth + 1, path)
    }
}

private fun actionsBefore(
    nodes: List<JsonElement>,
    container: String,
    target: String,
    found: BooleanArray,
): List<JsonElement> {
    val before = mutableListOf<JsonElement>()
    nodes.forEachIndexed { index, node ->
        if (found[0]) return before
        val path = "$container/$index"
        if (path == target) {
            found[0] = true
            return before
        }
        when (node.text("type")) {
            "if" -> {
                val then = actionsBefore(node.list("then"), "$path/then", target, found)
                if (found[0]) return before + then
                val otherwise =
                    actionsBefore(node.list("otherwise"), "$path/otherwise", target, found)
                if (found[0]) return before + otherwise
                before += then + otherwise
            }
            "forEach" -> before += actionsBefore(node.list("body"), "$path/body", target, found)
            else -> before += node
        }
    }
    return before
}

private fun rememberedRuleNames(nodes: List<JsonElement>): List<String> =
    nodes
        .flatMap { node ->
            when (node.text("type")) {
                "if" ->
                    rememberedRuleNames(node.list("then")) +
                        rememberedRuleNames(node.list("otherwise"))
                "forEach" -> rememberedRuleNames(node.list("body"))
                else ->
                    if (
                        node.obj("action").text("type") == "storeValue" &&
                            node.obj("action").text("scope") == "rule"
                    )
                        listOf(node.obj("action").text("name"))
                    else emptyList()
            }
        }
        .filter { it.isNotBlank() }
        .distinct()

fun ruleVariablesAt(
    definition: JsonElement,
    path: String,
    catalog: HookRuleCatalog?,
): List<RuleVariable> {
    val hook =
        catalog?.hooks?.firstOrNull {
            triggerKey(wireJson.encodeToJsonElement(it.trigger)) ==
                triggerKey(definition.obj("trigger"))
        }
    val fields =
        schemaFields(hook?.payloadSchema, "payload").map { field ->
            field.copy(
                personId =
                    hook?.resourceFields?.any { resource ->
                        resource.path == field.path &&
                            (resource.resourceType.contains("person", true) ||
                                resource.resourceType.contains("user", true) ||
                                resource.path.contains("author", true) ||
                                resource.path.contains("sender", true))
                    } == true
            )
        }
    val earlier = actionsBefore(definition.list("steps"), "/steps", path, BooleanArray(1))
    val remembered =
        rememberedRuleNames(definition.list("steps")).map { name ->
            RuleVariable(
                "Mémorisées",
                "values[${JsonPrimitive(name)}]",
                name,
                "valeur",
                "Valeur mémorisée par cette règle.",
            )
        }
    val loops = mutableListOf<RuleVariable>()
    val parts = segments(path)
    var nodes: JsonElement? = (definition as? JsonObject)?.get("steps")
    var index = 1
    while (index < parts.size) {
        val node = (nodes as? JsonArray)?.getOrNull(parts[index].toIntOrNull() ?: -1)
        if (node.text("type") == "forEach" && parts.getOrNull(index + 1) == "body")
            loops +=
                RuleVariable(
                    "Boucle",
                    node.text("itemName"),
                    node.text("itemName"),
                    "élément",
                    "L’élément courant de ${describeValue(node.obj("items"))}.",
                )
        nodes = (node as? JsonObject)?.get(parts.getOrNull(index + 1).orEmpty())
        index += 2
    }
    return fields +
        earlier.map { node ->
            RuleVariable(
                "Étapes précédentes",
                "steps.${node.text("id")}",
                node.text("label").ifBlank { node.text("id") },
                "résultat",
                "Le résultat de « ${node.text("id")} ».",
            )
        } +
        loops.asReversed().distinctBy { it.expression } +
        remembered +
        (catalog?.variables?.map {
            RuleVariable("Contexte", it.name, it.name, "valeur", it.description)
        }
            ?: listOf(
                RuleVariable(
                    "Contexte",
                    "values",
                    "Valeurs mémorisées",
                    "valeur",
                    "Les valeurs de cette règle.",
                ),
                RuleVariable("Contexte", "now", "now", "date", "Le début du déclenchement."),
                RuleVariable("Contexte", "event", "event", "objet", "Métadonnées de l’événement."),
            )) +
        listOf("id", "hook", "instanceId", "receivedAt").map {
            RuleVariable(
                "Contexte",
                "event.$it",
                "event.$it",
                "valeur",
                "Métadonnées de l’événement.",
            )
        } +
        catalog?.functions.orEmpty().map {
            RuleVariable("Fonctions", it.example, it.signature, "ƒ", it.description, it.example)
        }
}

/** A JSON literal's number, for amounts such as a wait. */
fun JsonElement?.number(): Double? = (this as? JsonPrimitive)?.doubleOrNull

fun JsonElement?.flag(): Boolean? = (this as? JsonPrimitive)?.booleanOrNull

fun relativeStamp(at: Long, now: Long = System.currentTimeMillis()): String {
    val seconds = (now - at) / 1000
    return when {
        seconds < 45 -> "à l’instant"
        seconds < 3_600 -> "il y a ${seconds / 60} min"
        seconds < 86_400 -> "il y a ${seconds / 3_600} h"
        else ->
            java.text.DateFormat.getDateInstance(
                    java.text.DateFormat.MEDIUM,
                    java.util.Locale.FRENCH,
                )
                .format(java.util.Date(at))
    }
}

internal fun JsonObject?.orEmpty(): JsonObject = this ?: JsonObject(emptyMap())

internal fun JsonElement.asObject(): JsonObject = jsonObject

internal fun JsonElement.asArray(): JsonArray = jsonArray

private const val RULE_SCHEMA_NULLABLE = "_cairnAllowsNull"
private const val RULE_SCHEMA_JSON = "_cairnJsonWidget"

/** A presentation index of composed tool inputs; the plugin validates the full schema. */
fun ruleToolInputSchema(root: JsonObject): JsonObject {
    val budget = intArrayOf(512)
    fun collect(schema: JsonObject, depth: Int): JsonObject {
        if (depth > 8 || budget[0]-- <= 0) return JsonObject(emptyMap())
        val reference = schema.text("\$ref")
        if (reference.startsWith("#/")) {
            var target: JsonElement? = root
            for (part in reference.removePrefix("#/").split('/')) {
                target = (target as? JsonObject)?.get(part.replace("~1", "/").replace("~0", "~"))
            }
            return collect(
                JsonObject((target as? JsonObject).orEmpty() + schema.filterKeys { it != "\$ref" }),
                depth + 1,
            )
        }
        val properties = schema.obj("properties").orEmpty().toMutableMap()
        val required = schema.list("required").map { it.jsonPrimitive.content }.toMutableSet()
        fun merge(fields: JsonObject?, composition: String = "allOf") {
            for ((name, field) in fields.orEmpty()) {
                val previous = properties[name]
                properties[name] =
                    if (previous != null && previous != field)
                        buildJsonObject {
                            (previous as? JsonObject)?.get("title")?.let { put("title", it) }
                            put(composition, JsonArray(listOf(previous, field)))
                            put(RULE_SCHEMA_JSON, true)
                        }
                    else field
            }
        }
        for (branch in schema.list("allOf")) {
            val fields = collect(branch as? JsonObject ?: JsonObject(emptyMap()), depth + 1)
            merge(fields.obj("properties"))
            required.addAll(fields.list("required").map { it.jsonPrimitive.content })
        }
        for (kind in listOf("oneOf", "anyOf")) {
            val branches =
                schema.list(kind).map {
                    collect(it as? JsonObject ?: JsonObject(emptyMap()), depth + 1)
                }
            for (branch in branches) merge(branch.obj("properties"), "anyOf")
            val common =
                branches.firstOrNull().list("required").filter { name ->
                    branches.all { name in it.list("required") }
                }
            required.addAll(common.map { it.jsonPrimitive.content })
        }
        for (name in required) {
            if (name !in properties)
                properties[name] = JsonObject(mapOf(RULE_SCHEMA_JSON to JsonPrimitive(true)))
        }
        return JsonObject(
            mapOf(
                "properties" to JsonObject(properties),
                "required" to JsonArray(required.map(::JsonPrimitive)),
            )
        )
    }
    return JsonObject(root + collect(root, 0))
}

fun resolveRuleSchema(schema: JsonObject, root: JsonObject = schema, depth: Int = 0): JsonObject =
    resolveRuleSchemaBounded(schema, root, depth, intArrayOf(512))

private fun resolveRuleSchemaBounded(
    schema: JsonObject,
    root: JsonObject,
    depth: Int,
    budget: IntArray,
): JsonObject {
    if (depth > 8 || budget[0]-- <= 0) return JsonObject(emptyMap())
    val reference = schema.text("\$ref")
    if (reference.startsWith("#/")) {
        var target: JsonElement? = root
        reference.removePrefix("#/").split('/').forEach { part ->
            target = (target as? JsonObject)?.get(part.replace("~1", "/").replace("~0", "~"))
        }
        return resolveRuleSchemaBounded(
            JsonObject((target as? JsonObject).orEmpty() + schema.filterKeys { it != "\$ref" }),
            root,
            depth + 1,
            budget,
        )
    }
    val alternatives = schema.list("oneOf").ifEmpty { schema.list("anyOf") }
    val resolvedAlternatives = alternatives.map {
        resolveRuleSchemaBounded(
            it as? JsonObject ?: JsonObject(emptyMap()),
            root,
            depth + 1,
            budget,
        )
    }
    val jsonWidget =
        schema[RULE_SCHEMA_JSON] == JsonPrimitive(true) ||
            schema.list("type").count { it != JsonPrimitive("null") } > 1 ||
            resolvedAlternatives.count { it.text("type") != "null" } > 1
    val nullMatches = resolvedAlternatives.count { it[RULE_SCHEMA_NULLABLE] == JsonPrimitive(true) }
    val declaredNull =
        if (RULE_SCHEMA_NULLABLE in schema) schema[RULE_SCHEMA_NULLABLE] == JsonPrimitive(true)
        else
            schema["type"] == null ||
                schema.text("type") == "null" ||
                schema.list("type").any { it == JsonPrimitive("null") }
    val nullable =
        declaredNull &&
            (alternatives.isEmpty() ||
                if (schema["oneOf"] != null) nullMatches == 1 else nullMatches > 0) &&
            schema.list("allOf").all {
                resolveRuleSchemaBounded(
                    it as? JsonObject ?: JsonObject(emptyMap()),
                    root,
                    depth + 1,
                    budget,
                )[RULE_SCHEMA_NULLABLE] == JsonPrimitive(true)
            } &&
            (schema["enum"] == null || JsonNull in schema.list("enum")) &&
            (schema["const"] == null || schema["const"] == JsonNull)
    if (schema["type"] == null && alternatives.isNotEmpty()) {
        val chosen =
            resolvedAlternatives.firstOrNull { it.text("type") != "null" }
                ?: resolvedAlternatives.firstOrNull()
        return resolveRuleSchemaBounded(
            JsonObject(
                schema.filterKeys { it !in setOf("oneOf", "anyOf") } +
                    chosen.orEmpty() +
                    (RULE_SCHEMA_NULLABLE to JsonPrimitive(nullable)) +
                    (RULE_SCHEMA_JSON to JsonPrimitive(jsonWidget))
            ),
            root,
            depth + 1,
            budget,
        )
    }
    val resolved = schema.toMutableMap()
    // Preserve null intent after choosing a concrete widget type from a nullable union.
    resolved.remove(RULE_SCHEMA_NULLABLE)
    if (nullable || schema[RULE_SCHEMA_NULLABLE] == JsonPrimitive(false))
        resolved[RULE_SCHEMA_NULLABLE] = JsonPrimitive(nullable)
    if (jsonWidget) resolved[RULE_SCHEMA_JSON] = JsonPrimitive(true)
    schema.obj("properties")?.let { fields ->
        resolved["properties"] =
            JsonObject(
                fields.mapValues { (_, field) ->
                    resolveRuleSchemaBounded(
                        field as? JsonObject ?: JsonObject(emptyMap()),
                        root,
                        depth + 1,
                        budget,
                    )
                }
            )
    }
    schema.obj("items")?.let {
        resolved["items"] = resolveRuleSchemaBounded(it, root, depth + 1, budget)
    }
    (schema["type"] as? JsonArray)?.let { types ->
        resolved["type"] =
            types.firstOrNull { (it as? JsonPrimitive)?.content != "null" } ?: JsonPrimitive("null")
    }
    return JsonObject(resolved)
}

fun ruleSchemaAllowsNull(schema: JsonObject): Boolean =
    resolveRuleSchema(schema)[RULE_SCHEMA_NULLABLE] == JsonPrimitive(true)

fun ruleSchemaUsesJson(schema: JsonObject): Boolean =
    resolveRuleSchema(schema)[RULE_SCHEMA_JSON] == JsonPrimitive(true)

fun concreteRuleLiteralDefault(schema: JsonObject): JsonElement? {
    val resolved = resolveRuleSchema(schema)
    if (resolved.text("type") == "null") return null
    resolved["const"]?.let {
        return it.takeUnless { it == JsonNull }
    }
    if (resolved["enum"] != null) return resolved.list("enum").firstOrNull { it != JsonNull }
    resolved["default"]
        ?.takeUnless { it == JsonNull }
        ?.let {
            return it
        }
    return defaultRuleLiteral(JsonObject(resolved.filterKeys { it != "default" }))
}

fun defaultRuleLiteral(schema: JsonObject, depth: Int = 0): JsonElement {
    schema["const"]?.let {
        return it
    }
    schema["default"]?.let {
        return it
    }
    schema.list("enum").firstOrNull()?.let {
        return it
    }
    return when (schema.text("type")) {
        "null" -> JsonNull
        "boolean" -> JsonPrimitive(false)
        "integer",
        "number" -> JsonPrimitive(0)
        "object" ->
            if (depth >= 8) JsonObject(emptyMap())
            else
                JsonObject(
                    schema
                        .list("required")
                        .mapNotNull { key ->
                            val name = key.jsonPrimitive.content
                            schema.obj("properties").obj(name)?.let {
                                name to defaultRuleLiteral(it, depth + 1)
                            }
                        }
                        .toMap()
                )
        "array" -> JsonArray(emptyList())
        else -> JsonPrimitive("")
    }
}

fun defaultRuleInput(schema: JsonObject): JsonObject =
    if (
        schema.text("type") in setOf("boolean", "integer", "number", "object", "array", "null") ||
            schema["default"] != null ||
            schema["const"] != null ||
            schema["enum"] != null
    )
        literalRuleValue(defaultRuleLiteral(schema))
    else templateValue()

fun literalRuleValue(value: JsonElement): JsonObject = buildJsonObject {
    put("type", "literal")
    put("value", value)
}

private fun presentRuleFields(value: JsonObject): JsonObject =
    JsonObject(value.filterValues { it != JsonNull })

private fun canonicalRuleCondition(value: JsonElement): JsonElement {
    val objectValue = value as? JsonObject ?: return value
    val fields = presentRuleFields(objectValue).toMutableMap()
    when (value.text("type")) {
        "all",
        "any" ->
            fields["conditions"] = JsonArray(value.list("conditions").map(::canonicalRuleCondition))
        "not" -> value.obj("condition")?.let { fields["condition"] = canonicalRuleCondition(it) }
    }
    return JsonObject(fields)
}

private fun canonicalRuleNode(value: JsonElement): JsonElement {
    val objectValue = value as? JsonObject ?: return value
    val fields = presentRuleFields(objectValue).toMutableMap()
    if ((fields["label"] as? JsonPrimitive)?.contentOrNull == "") fields.remove("label")
    when (value.text("type")) {
        "if" -> {
            value.obj("condition")?.let { fields["condition"] = canonicalRuleCondition(it) }
            fields["then"] = JsonArray(value.list("then").map(::canonicalRuleNode))
            fields["otherwise"] = JsonArray(value.list("otherwise").map(::canonicalRuleNode))
        }
        "forEach" -> fields["body"] = JsonArray(value.list("body").map(::canonicalRuleNode))
        "action" -> value.obj("action")?.let { fields["action"] = presentRuleFields(it) }
    }
    return JsonObject(fields)
}

fun sameRuleDefinition(left: JsonObject, right: JsonObject): Boolean {
    fun canonical(value: JsonObject) =
        JsonObject(value + ("steps" to JsonArray(value.list("steps").map(::canonicalRuleNode))))
    return canonical(left) == canonical(right)
}

fun ruleConditionDepth(condition: JsonElement?): Int =
    when (condition.text("type")) {
        "all",
        "any" -> 1 + (condition.list("conditions").maxOfOrNull { ruleConditionDepth(it) } ?: 0)
        "not" -> 1 + ruleConditionDepth(condition.obj("condition"))
        else -> 1
    }

fun unwrapRuleTemplate(text: String): String {
    val trimmed = text.trim()
    if (trimmed.startsWith("{{") && trimmed.endsWith("}}")) {
        val expression = trimmed.substring(2, trimmed.length - 2)
        if (!expression.contains("{{")) return expression.trim()
    }
    return JsonPrimitive(text).toString()
}

fun authorVariable(variables: List<RuleVariable>): String {
    val strings = variables.filter { it.group == "Événement" && it.type == "string" }
    return strings.firstOrNull { it.personId }?.expression
        ?: strings
            .firstOrNull {
                Regex("""(?:^|\.)author(?:\.id|Id)$""", RegexOption.IGNORE_CASE)
                    .containsMatchIn(it.expression)
            }
            ?.expression
        ?: strings
            .firstOrNull {
                Regex("""(?:^|\.)author$""", RegexOption.IGNORE_CASE).containsMatchIn(it.expression)
            }
            ?.expression
        ?: if (
            variables.any {
                it.type == "object" &&
                    Regex("""(?:^|\.)author$""", RegexOption.IGNORE_CASE)
                        .containsMatchIn(it.expression)
            }
        )
            ""
        else "payload.author"
}

fun comparisonExpects(left: JsonElement?, operator: String, variables: List<RuleVariable>): String {
    if (operator == "in") return "json"
    if (operator == "contains") {
        val field = variables.firstOrNull {
            it.expression == left.text("expression") ||
                it.expression == "payload.${left.text("expression")}"
        }
        val items =
            if (left.text("type") == "literal") (left as? JsonObject)?.get("value") as? JsonArray
            else null
        if (items != null || field?.type == "array") {
            val types =
                items
                    ?.map { item ->
                        when {
                            item is JsonPrimitive && item.isString -> "string"
                            item.flag() != null -> "boolean"
                            item.number() != null -> "number"
                            else -> "valeur"
                        }
                    }
                    ?.distinct()
            return when (if (items != null) types?.singleOrNull() else field?.itemType) {
                "number",
                "integer" -> "number"
                "boolean" -> "boolean"
                "string" -> "text"
                else -> "json"
            }
        }
        if (
            left.text("type") == "formula" &&
                (field == null || field.type in setOf("value", "valeur", "result", "résultat"))
        )
            return "json"
        return "text"
    }
    if (operator in setOf("matches", "startsWith", "endsWith")) return "text"
    val type =
        if (left.text("type") == "formula")
            variables
                .firstOrNull {
                    it.expression == left.text("expression") ||
                        it.expression == "payload.${left.text("expression")}"
                }
                ?.type
        else if (left.text("type") == "literal")
            when ((left as? JsonObject)?.get("value")) {
                JsonNull -> "value"
                is JsonPrimitive ->
                    if (((left as JsonObject)["value"] as JsonPrimitive).isString) "string"
                    else if (left["value"].flag() != null) "boolean"
                    else if (left["value"].number() != null) "number" else "string"
                is JsonArray -> "array"
                is JsonObject -> "object"
                else -> "value"
            }
        else "string"
    if (
        operator in setOf("equals", "notEquals") &&
            (type == null ||
                type in
                    setOf(
                        "array",
                        "object",
                        "value",
                        "valeur",
                        "result",
                        "résultat",
                        "item",
                        "élément",
                    ))
    )
        return "json"
    if (
        (type == null ||
            type == "value" ||
            type == "valeur" ||
            type == "result" ||
            type == "résultat") && operator in setOf("greaterThan", "lessThan")
    )
        return "number"
    return when (type) {
        "integer",
        "number" -> "number"
        "boolean" -> "boolean"
        else -> "text"
    }
}

private fun literalComparisonSchema(value: JsonElement?, depth: Int = 0): JsonObject {
    if (depth > 8 || value == null || value == JsonNull) return JsonObject(emptyMap())
    if (value is JsonArray) {
        val candidates = value.map { literalComparisonSchema(it, depth + 1) }.distinct()
        return buildJsonObject {
            put("type", "array")
            put("items", candidates.singleOrNull() ?: JsonObject(emptyMap()))
        }
    }
    return buildJsonObject {
        put(
            "type",
            when {
                value is JsonObject -> "object"
                value is JsonPrimitive && value.isString -> "string"
                value.flag() != null -> "boolean"
                else -> "number"
            },
        )
    }
}

fun comparisonListSchema(left: JsonElement?, variables: List<RuleVariable>): JsonObject {
    val field = variables.firstOrNull {
        it.expression == left.text("expression") ||
            it.expression == "payload.${left.text("expression")}"
    }
    val expects = comparisonExpects(left, "equals", variables)
    val itemSchema =
        if (left.text("type") == "literal") {
            literalComparisonSchema((left as? JsonObject)?.get("value"))
        } else
            field?.schema
                ?: buildJsonObject {
                    if (expects != "json") put("type", if (expects == "text") "string" else expects)
                }
    return buildJsonObject {
        put("type", "array")
        put("items", itemSchema)
    }
}

fun typedComparisonRight(
    value: JsonElement?,
    expects: String,
    listOnly: Boolean = true,
): JsonObject {
    val held = value as? JsonObject ?: templateValue()
    if (held.text("type") == "literal" && ruleValueJsonDrafts(held).isNotEmpty()) return held

    if (
        held.text("type") == "formula" ||
            (held.text("type") == "template" && expects == "json" && !listOnly)
    )
        return held
    if (held.text("type") == "template" && expects != "text") {
        Regex("""^\s*\{\{([^{}]+)\}\}\s*$""").matchEntire(held.text("text"))?.let {
            return buildJsonObject {
                put("type", "formula")
                put("expression", it.groupValues[1].trim())
            }
        }
    }
    val raw =
        if (held.text("type") == "literal") held["value"] else JsonPrimitive(held.text("text"))
    val text = (raw as? JsonPrimitive)?.contentOrNull.orEmpty()
    if (expects == "json" && !listOnly)
        return literalRuleValue(
            if (held.text("type") == "literal") raw ?: JsonNull
            else runCatching { wireJson.parseToJsonElement(text) }.getOrElse { JsonPrimitive(text) }
        )
    return when (expects) {
        "number" ->
            literalRuleValue(
                text.toLongOrNull()?.let(::JsonPrimitive)
                    ?: text.toDoubleOrNull()?.takeIf { it.isFinite() }?.let(::JsonPrimitive)
                    ?: JsonPrimitive(0)
            )
        "boolean" -> literalRuleValue(JsonPrimitive(text == "true"))
        "json" ->
            literalRuleValue(
                (raw as? JsonArray)
                    ?: (runCatching { wireJson.parseToJsonElement(text) }.getOrNull() as? JsonArray)
                    ?: JsonArray(emptyList())
            )
        else ->
            if (held.text("type") == "literal" && (raw !is JsonPrimitive || !raw.isString))
                literalRuleValue(
                    JsonPrimitive((raw as? JsonPrimitive)?.contentOrNull ?: raw.toString())
                )
            else held
    }
}

fun ruleChoicesWithCurrent(
    options: List<Pair<String, String>>,
    current: String,
    fallback: String,
): List<Pair<String, String>> =
    if (current.isBlank() || options.any { it.first == current }) options
    else options + (current to "$fallback (indisponible)")
