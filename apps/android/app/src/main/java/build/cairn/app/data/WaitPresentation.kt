package build.cairn.app.data

import kotlinx.serialization.json.*

// The existing timeline presenter consumes JSON. Keep that presentation seam
// while its owning conversion runs independently; no handwritten wire models.
private fun presentCondition(value: JsonObject): JsonObject {
    if (value["type"]?.jsonPrimitive?.content == "plugin") {
        return buildJsonObject {
            value["input"]?.jsonObject?.forEach { (name, field) -> put(name, field) }
            put("type", value.getValue("conditionType"))
            put("pluginId", value.getValue("pluginId"))
            value["projectId"]?.let { put("projectId", it) }
        }
    }
    if (value["type"]?.jsonPrimitive?.content == "group") {
        return JsonObject(value + ("conditions" to JsonArray(value.getValue("conditions").jsonArray.map { presentCondition(it.jsonObject) })))
    }
    return value
}

private fun presentResult(value: JsonElement): JsonElement = when(value) {
    is JsonArray -> JsonArray(value.map(::presentResult))
    is JsonObject -> {
        val extra=value["pluginResult"] as? JsonObject
        JsonObject((extra.orEmpty()+value).mapValues { (_, field) -> presentResult(field) })
    }
    else -> value
}

val ChatWakeup.conditionJson: JsonObject?
    get() = condition?.let { presentCondition(wireJson.encodeToJsonElement(it).jsonObject) }

val ChatWakeup.membersJson: JsonArray?
    get() = members?.let { presentResult(wireJson.encodeToJsonElement(it)).jsonArray }

val ChatWakeup.observationJson: JsonObject?
    get() = observation?.let { presentResult(wireJson.encodeToJsonElement(it)).jsonObject }

val ChatWakeup.displayJson: JsonObject?
    get() = display?.let { wireJson.encodeToJsonElement(it).jsonObject }
