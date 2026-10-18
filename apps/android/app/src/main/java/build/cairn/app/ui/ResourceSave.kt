package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import build.cairn.app.data.*
import java.util.UUID
import kotlinx.serialization.json.*

/** A creation attempt keeps its identity and original payload until its response is known. */
internal class ResourceSave(
    id: String,
    private val creationId: String = UUID.randomUUID().toString(),
    pending: String = "",
    pendingPath: String = "",
    attempted: Boolean = false,
    uncertain: Boolean = false,
) {
    var id by mutableStateOf(id)
        private set

    private var pending by mutableStateOf(pending)
    private var pendingPath = pendingPath
    private var attempted = attempted
    private var uncertain by mutableStateOf(uncertain)

    val pendingCreation: Boolean
        get() = pending.isNotBlank() || uncertain

    suspend fun save(
        api: CairnApi,
        path: String,
        value: () -> JsonElement,
        createPath: String = path,
        createValue: () -> JsonElement = value,
        onCreated: (JsonElement) -> Unit = {},
    ): JsonElement {
        if (id.isNotBlank()) return api.send("PUT", "$path/${segment(id)}", value())

        val retry = attempted
        attempted = true
        if (pending.isBlank()) {
            pending = createValue().toString()
            pendingPath = createPath
        }

        val original = wireJson.parseToJsonElement(pending)
        val creation = JsonObject(original.jsonObject + ("creationId" to JsonPrimitive(creationId)))
        val saved =
            try {
                val response = api.send<JsonElement>("POST", pendingPath, creation)
                val acquiredId =
                    response.jsonObject["id"]?.jsonPrimitive?.takeIf { it.isString }?.content
                check(!acquiredId.isNullOrBlank()) {
                    "Identifiant absent de la réponse de sauvegarde."
                }
                id = acquiredId
                uncertain = false
                response
            } catch (error: ApiException) {
                // A client rejection allows correcting this payload. Earlier attempts may
                // still have committed, so keep their identity, retry history and uncertainty.
                if (error.status in 400..499 && error.status != 408) pending = ""
                else uncertain = true
                throw error
            } catch (error: Exception) {
                uncertain = true
                throw error
            }

        pending = ""
        onCreated(saved)

        val latest = value()
        return if (retry) api.send("PUT", "$path/${segment(id)}", latest) else saved
    }

    companion object {
        val Saver =
            listSaver<ResourceSave, String>(
                save = {
                    listOf(
                        it.id,
                        it.creationId,
                        it.pending,
                        it.pendingPath,
                        it.attempted.toString(),
                        it.uncertain.toString(),
                    )
                },
                restore = {
                    ResourceSave(it[0], it[1], it[2], it[3], it[4].toBoolean(), it[5].toBoolean())
                },
            )
    }
}

@Composable
internal fun rememberResourceSave(id: String): ResourceSave =
    rememberSaveable(saver = ResourceSave.Saver) { ResourceSave(id) }
