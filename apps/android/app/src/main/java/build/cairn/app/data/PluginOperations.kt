package build.cairn.app.data

import android.os.SystemClock
import build.cairn.contracts.PluginOperationInput
import build.cairn.contracts.contractJson
import build.cairn.contracts.getPluginOperation
import kotlinx.coroutines.delay
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Poll a manager operation without holding a request past the relay deadline. */
suspend fun CairnApi.pluginOperation(
    path: String,
    body: JsonElement? = null,
    method: String = "POST",
): JsonElement {
    val result = wireJson.parseToJsonElement(request(method, path, body))
    val id = result.jsonObject["operation"]?.jsonPrimitive?.contentOrNull ?: return result
    val deadline = SystemClock.elapsedRealtime() + 10 * 60 * 1000
    while (SystemClock.elapsedRealtime() < deadline) {
        val operation = getPluginOperation(PluginOperationInput(id))
        when (operation.state) {
            "completed" -> {
                val result = contractJson.encodeToJsonElement(operation.result!!).jsonObject
                return when (result["kind"]?.jsonPrimitive?.content) {
                    "project" -> result.getValue("project")
                    "sourceProjects",
                    "resourcePage" -> result.getValue("page")
                    "resourceAncestry" -> result.getValue("resolution")
                    "skillImport" -> result.getValue("imported")
                    "skillSource" -> result.getValue("status")
                    "skillBundle" -> result.getValue("bundle")
                    "package" -> result.getValue("version")
                    "program" -> result.getValue("program")
                    "management" -> {
                        val output = result.getValue("output").jsonObject
                        if (result["wrapped"]?.jsonPrimitive?.content == "true") output
                        else
                            when (output["kind"]?.jsonPrimitive?.content) {
                                "form" -> output.getValue("form")
                                "saved",
                                "enabled",
                                "people" -> output.getValue("instance")
                                "action" -> output["result"] ?: JsonNull
                                "signin" -> output.getValue("flow")
                                else -> JsonObject(output - "kind")
                            }
                    }
                    else -> error("Unknown plugin operation result")
                }
            }
            "failed",
            "cancelled" ->
                throw ApiException(
                    operation.errorStatus?.toInt() ?: 502,
                    operation.error ?: "La préparation du plugin a échoué.",
                    explained = true,
                )
        }
        delay(500)
    }
    throw ApiException(
        504,
        "La préparation continue. Actualisez le plugin pour vérifier son état.",
        explained = true,
    )
}
