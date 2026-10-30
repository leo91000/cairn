package build.cairn.app.data

import kotlinx.serialization.json.*

fun nodeWireFixture(requestPath: String?, body: String): String =
    if (requestPath?.substringBefore('?')?.contains("/api/nodes") == true) nodeWireFixture(body)
    else body

/** Complete current node responses, built from the generated models for JVM/device setup. */
fun nodeWireFixture(body: String): String {
    val parsed = runCatching { wireJson.parseToJsonElement(body) }.getOrNull() ?: return body
    return completeNodeWire(parsed).toString()
}

private fun completeNodeWire(value: JsonElement): JsonElement {
    if (value is JsonArray) return JsonArray(value.map(::completeNodeWire))
    if (value !is JsonObject) return value
    val fields = value.mapValues { completeNodeWire(it.value) }.toMutableMap()
    val status = fields["status"]?.jsonPrimitive?.contentOrNull
    val isNode =
        fields["id"] != null &&
            fields["name"] != null &&
            (status in listOf("online", "offline", "local", "revoked") ||
                fields["capabilities"] != null)
    if (isNode) {
        // This symbolic node also names a backend; only its identity is expanded.
        if (fields["id"] == JsonPrimitive("native"))
            fields["id"] = JsonPrimitive("aa3fdfe2-c3db-5061-bfdc-7aeeeceb3503")
        val base =
            ExecutionNode(
                id = fields.getValue("id").jsonPrimitive.content,
                name = fields.getValue("name").jsonPrimitive.content,
                revision = "fixture",
                createdAt = 0,
                capabilities =
                    NodeCapabilities(
                        os = "linux",
                        arch = "x86_64",
                        kvm = true,
                        cpu = 8,
                        memoryMiB = 16384,
                        diskMiB = 65536,
                    ),
                limits = NodeResources(cpu = 4, memoryMiB = 8192, diskMiB = 32768),
                storage = NodeStoragePolicy(),
                staleDisks = StaleDisks(count = 0, diskMiB = 0),
            )
        val complete =
            mergeNodeFixture(wireJson.encodeToJsonElement(base).jsonObject, JsonObject(fields))
                .toMutableMap()
        (complete["nativeModeObserved"] as? JsonObject)?.let { observation ->
            complete["nativeModeObserved"] =
                JsonObject(
                    mapOf("generation" to JsonNull, "status" to JsonPrimitive("applied")) +
                        observation
                )
        }
        return wireJson.encodeToJsonElement(
            wireJson.decodeFromJsonElement<ExecutionNode>(JsonObject(complete))
        )
    }
    if (fields["code"] != null && fields["expiresAt"] != null && fields["nodeId"] != null) {
        fields.putIfAbsent("route", JsonPrimitive("direct"))
        fields.putIfAbsent("managerUrl", JsonNull)
        fields.putIfAbsent("backend", JsonPrimitive("firecracker"))
        return wireJson.encodeToJsonElement(
            wireJson.decodeFromJsonElement<NodeEnrollment>(JsonObject(fields))
        )
    }
    if (fields["previewId"] != null && fields["preview"] is JsonObject) {
        val impact = fields.getValue("preview").jsonObject.toMutableMap()
        impact.putIfAbsent("warning", JsonPrimitive("Review native host administration."))
        impact.putIfAbsent(
            "interruptions",
            JsonPrimitive("Managed executions stop before the new mode is applied."),
        )
        for (key in
            listOf("agents", "conversations", "tasks", "trustedAccounts", "trustedPeople")) impact
            .putIfAbsent(key, JsonArray(emptyList()))
        fields["preview"] = JsonObject(impact)
        fields["kind"] = JsonPrimitive("preview")
    }
    if (fields["releasedPlacements"] != null && fields["node"] == null) {
        fields["node"] =
            completeNodeWire(
                buildJsonObject {
                    put("id", "a9b05b99-dd0d-44c6-aa32-97614d7577bc")
                    put("name", "Retired node")
                    put("status", "revoked")
                    put("revoked", true)
                }
            )
    }
    return JsonObject(fields)
}

private fun mergeNodeFixture(base: JsonObject, patch: JsonObject): JsonObject =
    JsonObject(
        base.toMutableMap().apply {
            for ((key, value) in patch) {
                val previous = get(key)
                put(
                    key,
                    if (previous is JsonObject && value is JsonObject)
                        mergeNodeFixture(previous, value)
                    else value,
                )
            }
        }
    )
