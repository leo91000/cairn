package build.cairn.app.data

import build.cairn.contracts.AgentSettings
import build.cairn.contracts.contractJson
import kotlinx.serialization.json.*

/** Current wire profile returned by a command fixture, with explicit server metadata. */
fun agentFixture(settings: AgentSettings, id: String, revision: String): Agent =
    wireJson.decodeFromJsonElement(
        JsonObject(
            contractJson.encodeToJsonElement(settings).jsonObject +
                mapOf(
                    "id" to JsonPrimitive(id),
                    "revision" to JsonPrimitive(revision),
                    "createdAt" to JsonPrimitive(0),
                )
        )
    )
