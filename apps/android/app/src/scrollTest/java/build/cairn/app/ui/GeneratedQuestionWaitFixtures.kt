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

private fun normalizedResult(value: JsonObject): JsonObject {
    if (value["pluginId"] != null)
        return buildJsonObject {
            value
                .filterKeys { it in setOf("state", "type", "message", "display") }
                .forEach { (key, field) -> put(key, field) }
            put(
                "pluginResult",
                JsonObject(
                    value.filterKeys { it !in setOf("state", "type", "message", "display") }
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
        observation = observation?.let { wireJson.decodeFromJsonElement(normalizedResult(it)) },
        members = members?.let { wireJson.decodeFromJsonElement(it) },
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
