package build.cairn.app.data

import build.cairn.contracts.ManagePluginInput
import build.cairn.contracts.ManagePluginOutput
import build.cairn.contracts.PluginActionInput
import build.cairn.contracts.PluginConfiguration
import build.cairn.contracts.PluginEnabledInput
import build.cairn.contracts.PluginFormInput
import build.cairn.contracts.managePlugin
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject

suspend fun CairnApi.pluginForm(
    plugin: String,
    instance: String?,
    data: PluginFormInput,
): PluginForm =
    (managePlugin(ManagePluginInput.B(data = data, instanceId = instance, pluginId = plugin))
            as ManagePluginOutput.B)
        .form

suspend fun CairnApi.savePlugin(
    plugin: String,
    instance: String?,
    data: PluginConfiguration,
): PluginConnection =
    (managePlugin(ManagePluginInput.C(data = data, instanceId = instance, pluginId = plugin))
            as ManagePluginOutput.C)
        .instance

suspend fun CairnApi.enablePlugin(plugin: String, instance: String, enabled: Boolean) {
    managePlugin(
        ManagePluginInput.D(
            data = PluginEnabledInput(enabled),
            instanceId = instance,
            pluginId = plugin,
        )
    )
        as ManagePluginOutput.D
}

suspend fun CairnApi.deletePlugin(plugin: String, instance: String) {
    managePlugin(ManagePluginInput.E(instanceId = instance, pluginId = plugin))
        as ManagePluginOutput.E
}

suspend fun CairnApi.disconnectPlugin(plugin: String, instance: String): String =
    (managePlugin(ManagePluginInput.F(instanceId = instance, pluginId = plugin))
            as ManagePluginOutput.F)
        .message

suspend fun CairnApi.testPlugin(plugin: String, instance: String): String =
    (managePlugin(ManagePluginInput.G(instanceId = instance, pluginId = plugin))
            as ManagePluginOutput.G)
        .message

suspend fun CairnApi.pluginView(plugin: String, instance: String): JsonElement? =
    (managePlugin(ManagePluginInput.H(instanceId = instance, pluginId = plugin))
            as ManagePluginOutput.H)
        .view

suspend fun CairnApi.pluginAction(
    plugin: String,
    instance: String,
    action: String,
    payload: JsonElement,
    values: JsonObject,
): JsonElement =
    (managePlugin(
            ManagePluginInput.I(
                data = PluginActionInput(action = action, payload = payload, values = values),
                instanceId = instance,
                pluginId = plugin,
            )
        )
            as ManagePluginOutput.I)
        .result ?: JsonNull

suspend fun CairnApi.pluginPerson(
    plugin: String,
    instance: String,
    person: String,
    operation: String,
) {
    managePlugin(
        ManagePluginInput.M(
            pluginId = plugin,
            instanceId = instance,
            userId = person,
            operation = operation,
        )
    )
        as ManagePluginOutput.L
}
