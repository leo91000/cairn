package build.cairn.app.data

import build.cairn.contracts.HookRuleProblem
import kotlinx.serialization.json.*

private const val JSON_DRAFTS = "_jsonDrafts"
private val JSON_NUMBER = Regex("-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][+-]?[0-9]+)?")

/** The tree parser accepts arbitrary unquoted primitives; editor JSON must not. */
fun parseRuleJsonInput(text: String): JsonElement {
    val value = wireJson.parseToJsonElement(text)
    fun check(current: JsonElement) {
        when (current) {
            is JsonObject -> current.values.forEach(::check)
            is JsonArray -> current.forEach(::check)
            is JsonPrimitive ->
                require(
                    current.isString ||
                        current == JsonNull ||
                        current.booleanOrNull != null ||
                        (JSON_NUMBER.matches(current.content) &&
                            current.content.toDoubleOrNull()?.isFinite() == true)
                ) {
                    "Invalid JSON primitive"
                }
        }
    }
    check(value)
    return value
}

private fun draftSegment(key: Any) = "/${key.toString().replace("~", "~0").replace("/", "~1")}"

/** UI-only syntax state survives closing a step and recovering its draft. */
fun ruleValueJsonDrafts(value: JsonElement?): JsonObject =
    JsonObject(
        value.obj(JSON_DRAFTS).orEmpty().filterValues {
            it is JsonObject &&
                (it["text"] as? JsonPrimitive)?.isString == true &&
                (it["message"] as? JsonPrimitive)?.isString == true
        }
    )

fun jsonInputDraft(text: String, message: String = "Saisissez une valeur JSON valide.") =
    buildJsonObject {
        put(
            "",
            buildJsonObject {
                put("text", text)
                put("message", message)
            },
        )
    }

fun literalWithJsonDrafts(value: JsonElement, drafts: JsonObject = JsonObject(emptyMap())) =
    buildJsonObject {
        put("type", "literal")
        put("value", value)
        if (drafts.isNotEmpty()) put(JSON_DRAFTS, drafts)
    }

fun childJsonDrafts(drafts: JsonObject, key: Any): JsonObject {
    val prefix = draftSegment(key)
    return JsonObject(
        drafts
            .filterKeys { it == prefix || it.startsWith("$prefix/") }
            .mapKeys { it.key.removePrefix(prefix) }
    )
}

fun replaceChildJsonDrafts(drafts: JsonObject, key: Any, child: JsonObject): JsonObject {
    val prefix = draftSegment(key)
    return JsonObject(
        drafts.filterKeys { it != prefix && !it.startsWith("$prefix/") } +
            child.mapKeys { "$prefix${it.key}" }
    )
}

fun removeJsonDraftItem(drafts: JsonObject, index: Int): JsonObject = buildJsonObject {
    for ((path, raw) in drafts) {
        val match = Regex("^/(\\d+)(/.*)?$").matchEntire(path)
        if (match == null) {
            put(path, raw)
            continue
        }
        val at = match.groupValues[1].toIntOrNull()
        if (at == null) put(path, raw)
        else if (at != index) put("/${if (at > index) at - 1 else at}${match.groupValues[2]}", raw)
    }
}

fun ruleJsonDraftProblems(value: JsonElement): List<HookRuleProblem> {
    val problems = mutableListOf<HookRuleProblem>()
    fun visit(current: JsonElement, path: String) {
        when (current) {
            is JsonArray -> current.forEachIndexed { index, child -> visit(child, "$path/$index") }
            is JsonObject -> {
                if (current.text("type") == "literal") {
                    for ((suffix, draft) in ruleValueJsonDrafts(current)) problems +=
                        HookRuleProblem(
                            path = "$path/value$suffix",
                            message = draft.text("message"),
                        )
                    return // Literal data may itself resemble rule syntax.
                }
                for ((key, child) in current) visit(child, "$path${draftSegment(key)}")
            }
            else -> Unit
        }
    }
    visit(value, "")
    return problems
}
