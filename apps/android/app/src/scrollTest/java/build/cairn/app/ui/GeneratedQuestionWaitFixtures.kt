package build.cairn.app.ui

import build.cairn.app.data.*
import kotlinx.serialization.json.*

fun optionFixture(
    label: String,
    description: String = "",
    id: String = "",
    recommended: Boolean = false,
) = QuestionOption(id = id, label = label, description = description, recommended = recommended)

fun fieldFixture(
    id: String,
    title: String,
    secret: Boolean = false,
    options: List<QuestionOption> = emptyList(),
    mode: String = "single",
    minChoices: Int? = null,
    maxChoices: Int? = null,
) =
    QuestionField(
        id = id,
        title = title,
        secret = secret,
        options = options,
        mode = mode,
        minChoices = minChoices,
        maxChoices = maxChoices,
    )

fun answerFixture(selected: List<String> = emptyList(), comment: String = "") =
    QuestionAnswer(selected = selected, comment = comment)

fun questionFixture(
    id: String,
    chatId: String,
    runId: String = "",
    blocking: Boolean = false,
    fields: List<QuestionField> = emptyList(),
    status: String = "pending",
    createdAt: Long = 0,
) =
    ChatQuestion(
        id = id,
        chatId = chatId,
        runId = runId,
        blocking = blocking,
        fields = fields,
        status = status,
        createdAt = createdAt,
        source = "cairn",
    )

private fun normalizedCondition(value: JsonObject): JsonObject {
    if (value["type"]?.jsonPrimitive?.content == "group")
        return JsonObject(
            value +
                ("conditions" to
                    JsonArray(
                        value.getValue("conditions").jsonArray.map {
                            normalizedCondition(it.jsonObject)
                        }
                    ))
        )
    if (value["pluginId"] != null && value["type"]?.jsonPrimitive?.content != "plugin")
        return buildJsonObject {
            put("type", "plugin")
            put("conditionType", value.getValue("type"))
            put("pluginId", value.getValue("pluginId"))
            value["projectId"]?.let { put("projectId", it) }
            put(
                "input",
                JsonObject(value.filterKeys { it !in setOf("type", "pluginId", "projectId") }),
            )
        }
    return value
}

fun waitResultFixture(value: JsonObject): JsonObject {
    if (value["type"]?.jsonPrimitive?.content == "group")
        return JsonObject(
            value +
                ("members" to
                    JsonArray(
                        value.getValue("members").jsonArray.map { item ->
                            val member = item.jsonObject
                            JsonObject(
                                member.mapValues { (key, field) ->
                                    when {
                                        key == "condition" && field is JsonObject ->
                                            normalizedCondition(field)
                                        key == "result" && field is JsonObject ->
                                            waitResultFixture(field)
                                        else -> field
                                    }
                                }
                            )
                        }
                    ))
        )
    if (value["pluginResult"] != null) return value
    if (
        value["pluginId"] != null ||
            value["type"]?.jsonPrimitive?.content?.startsWith("github_") == true
    )
        return buildJsonObject {
            value
                .filterKeys {
                    it in setOf("state", "type", "message", "display", "reason", "immediate")
                }
                .forEach { (key, field) -> put(key, field) }
            put(
                "pluginResult",
                JsonObject(
                    value.filterKeys {
                        it !in setOf("state", "type", "message", "display", "reason", "immediate")
                    }
                ),
            )
        }
    return value
}

fun wakeupFixture(
    id: String,
    messageId: String? = null,
    reason: String,
    seconds: Long,
    createdAt: Long,
    wakeAt: Long,
    condition: JsonObject? = null,
    display: JsonObject? = null,
    observation: JsonObject? = null,
    members: JsonArray? = null,
    timeCondition: JsonObject? = null,
    nextCheckAt: Long? = null,
    lastError: String? = null,
) =
    ChatWakeup(
        id = id,
        messageId = messageId,
        reason = reason,
        seconds = seconds,
        createdAt = createdAt,
        wakeAt = wakeAt,
        condition = condition?.let { wireJson.decodeFromJsonElement(normalizedCondition(it)) },
        display = display?.let { wireJson.decodeFromJsonElement(it) },
        observation = observation?.let { wireJson.decodeFromJsonElement(waitResultFixture(it)) },
        members =
            members?.let { value ->
                wireJson.decodeFromJsonElement(
                    JsonArray(
                        value.map { item ->
                            val member = item.jsonObject
                            JsonObject(
                                member.mapValues { (key, field) ->
                                    if (key == "observation" && field is JsonObject)
                                        waitResultFixture(field)
                                    else field
                                }
                            )
                        }
                    )
                )
            },
        timeCondition = timeCondition?.let { wireJson.decodeFromJsonElement(it) },
        nextCheckAt = nextCheckAt,
        lastError = lastError,
    )

fun ChatWakeup.withWaitProgress(
    id: String = this.id,
    messageId: String? = this.messageId,
    createdAt: Long = this.createdAt,
    condition: JsonObject? = this.conditionJson,
    display: JsonObject? = this.displayJson,
    observation: JsonObject? = this.observationJson,
    members: JsonArray? = this.membersJson,
) =
    wakeupFixture(
        id,
        messageId,
        reason,
        seconds,
        createdAt,
        wakeAt,
        condition,
        display,
        observation,
        members,
    )
