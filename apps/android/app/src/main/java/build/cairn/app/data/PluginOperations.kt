package build.cairn.app.data

import android.os.SystemClock
import kotlinx.coroutines.delay
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Poll a manager operation without holding a request past the relay deadline. */
suspend fun CairnApi.pluginOperation(path: String, body: JsonElement): JsonElement {
    val result = wireJson.parseToJsonElement(request("POST", path, body))
    val id = result.jsonObject["operation"]?.jsonPrimitive?.contentOrNull ?: return result
    val deadline = SystemClock.elapsedRealtime() + 10 * 60 * 1000
    while (SystemClock.elapsedRealtime() < deadline) {
        val operation = wireJson.parseToJsonElement(request("GET", "/plugins/packages/operations/${segment(id)}")).jsonObject
        when (operation["state"]?.jsonPrimitive?.content) {
            "completed" -> return operation["result"] ?: JsonNull
            "failed" -> throw ApiException(502, operation["error"]?.jsonPrimitive?.content ?: "La préparation du plugin a échoué.", explained = true)
        }
        delay(500)
    }
    throw ApiException(504, "La préparation continue. Actualisez le plugin pour vérifier son état.", explained = true)
}
