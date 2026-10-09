package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

@Composable
fun AuthorizeScreen(vm: CairnViewModel, state: Workspace, sharedUrl: String, consumed: () -> Unit) {
    var url by rememberSaveable { mutableStateOf(sharedUrl) }
    var preview by remember { mutableStateOf<ConsentPreview?>(null) }
    var parameters by remember { mutableStateOf<Map<String, String>>(emptyMap()) }
    var redirect by remember { mutableStateOf("") }
    LaunchedEffect(sharedUrl) {
        if (sharedUrl.isNotBlank()) {
            url = sharedUrl
            preview = null
            redirect = ""
            consumed()
        }
    }
    Page {
        Heading(
            "Autoriser un assistant",
            "Vérifiez ce que le client pourra faire dans votre espace.",
        )
        Text(
            "Partagez une page d’autorisation Cairn vers cette application depuis votre navigateur, ou collez son lien ici."
        )
        Field(
            "Lien d’autorisation",
            url,
            {
                url = it
                preview = null
                redirect = ""
            },
            keyboardOptions = InputKeyboards.Uri,
        )
        Button(
            onClick = {
                vm.perform {
                    parameters = authorizationParameters(url, api.origin)
                    preview =
                        api.send(
                            "POST",
                            "/mcp/oauth/preview",
                            wireJson.encodeToJsonElement(parameters),
                        )
                }
            },
            enabled = url.isNotBlank() && !state.busy && redirect.isEmpty(),
        ) {
            Text("Examiner la demande")
        }
        preview?.let { details ->
            Panel {
                Text(details.client.client_name, style = MaterialTheme.typography.titleLarge)
                details.scopes.forEach { scope ->
                    Text(
                        when (scope) {
                            "read" -> "Lire les tâches, les profils, les skills et les résultats."
                            "run" ->
                                "Lancer et arrêter des tâches avec les accès complets du worker."
                            "manage" ->
                                "Créer et modifier les tâches, les projets, les agents et les skills."
                            else -> scope
                        }
                    )
                }
                if (redirect.isEmpty())
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        listOf(false to "Refuser", true to "Autoriser").forEach { (approved, label)
                            ->
                            OutlinedButton(
                                onClick = {
                                    vm.perform {
                                        redirect =
                                            api.send<ConsentResult>(
                                                    "POST",
                                                    "/mcp/oauth/consent",
                                                    buildJsonObject {
                                                        put(
                                                            "parameters",
                                                            wireJson.encodeToJsonElement(
                                                                parameters
                                                            ),
                                                        )
                                                        put("approved", approved)
                                                        put(
                                                            "installationId",
                                                            checkNotNull(state.installation).id,
                                                        )
                                                    },
                                                )
                                                .redirect
                                    }
                                },
                                enabled = !state.busy,
                            ) {
                                Text(label)
                            }
                        }
                    }
                else {
                    Text(
                        "La décision est enregistrée. Retournez au client pour terminer la connexion."
                    )
                    ExternalButton("Retourner au client", redirect)
                }
            }
        }
    }
}
