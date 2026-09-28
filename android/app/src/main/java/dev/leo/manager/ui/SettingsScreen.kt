package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.leo.manager.data.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.put

@Composable
fun SettingsScreen(vm: LeoViewModel, state: Workspace) {
    val theme by vm.theme.collectAsStateWithLifecycle(initialValue = "system")
    var settings by remember { mutableStateOf<Settings?>(null) }
    var grants by remember { mutableStateOf<List<Grant>>(emptyList()) }
    var audit by remember { mutableStateOf<List<Audit>>(emptyList()) }
    var creating by rememberSaveable { mutableStateOf(false) }
    var revoke by rememberSaveable { mutableStateOf<String?>(null) }
    var logout by remember { mutableStateOf(false) }
    suspend fun load() {
        settings = vm.api.get("/settings")
        grants = vm.api.get("/tokens")
        audit = vm.api.get("/audit")
    }
    Poll("settings", 30000) {
        try {
            load()
        } catch (e: Exception) {
            vm.report(e)
        }
    }
    Page {
        Heading("Paramètres", "Votre espace, vos règles.")
        Panel {
            Text("Apparence", style = MaterialTheme.typography.titleLarge)
            Choice(
                "Thème",
                theme,
                listOf("system" to "Système", "light" to "Clair", "dark" to "Sombre"),
                vm::setTheme,
            )
        }
        AppUpdateSettings()
        NotificationSettings(vm)
        settings?.let { info ->
            Panel {
                Text("Connecter un assistant", style = MaterialTheme.typography.titleLarge)
                Text(
                    "Ajoutez cette adresse comme serveur MCP dans votre assistant, puis choisissez l’authentification OAuth."
                )
                Code(info.mcpUrl)
                CopyButton("Copier l’adresse MCP", info.mcpUrl)
                Text("Protocole : ${info.protocol}")
            }
            Panel {
                Text("Clients et jetons d’accès", style = MaterialTheme.typography.titleLarge)
                Button(
                    onClick = {
                        vm.clearMessage()
                        creating = true
                    }
                ) {
                    Text("Créer un jeton")
                }
                if (grants.isEmpty()) Text("Aucun accès accordé.")
                grants.forEach { grant ->
                    Text(grant.label, style = MaterialTheme.typography.titleMedium)
                    Text(
                        grant.scopes.joinToString(" · ") +
                            if (grant.clientId == "personal") " · Personnel" else " · OAuth"
                    )
                    TextButton(
                        onClick = {
                            vm.clearMessage()
                            revoke = grant.id
                        },
                        enabled = !state.busy,
                    ) {
                        Text("Révoquer")
                    }
                    HorizontalDivider()
                }
            }
            Panel {
                Text("Serveur", style = MaterialTheme.typography.titleLarge)
                Code(info.publicUrl)
                Text("Version ${info.version} · ${info.concurrency} workers")
                Code("Commit : ${info.commit}")
                Text("Répertoire du worker")
                Code(info.home)
                Text("Racines des projets")
                info.workspaceRoots.forEach { Code(it) }
                Text("Application Android ${dev.leo.manager.BuildConfig.VERSION_NAME}")
                TextButton(
                    onClick = {
                        vm.clearMessage()
                        logout = true
                    },
                    enabled = !state.busy,
                ) {
                    Text("Se déconnecter")
                }
            }
        } ?: LinearProgressIndicator(Modifier.fillMaxWidth())
        Text("Journal d’audit", style = MaterialTheme.typography.titleLarge)
        OutlinedButton(onClick = { vm.perform { load() } }, enabled = !state.busy) {
            Text("Actualiser")
        }
        if (audit.isEmpty()) Text("Aucun événement.")
        audit.forEach { entry ->
            Panel {
                Text(entry.action, style = MaterialTheme.typography.titleMedium)
                Text(date(entry.at), style = MaterialTheme.typography.labelMedium)
                Code(entry.detail)
            }
        }
    }
    if (creating) TokenEditor(vm, state, close = { creating = false }) { load() }
    revoke?.let { id ->
        Confirm(
            "Révoquer cet accès ?",
            "Ce client ne pourra plus utiliser ce jeton pour accéder à votre espace.",
            state.busy,
            state.error,
            { revoke = null },
        ) {
            vm.perform {
                api.request("DELETE", "/tokens/${segment(id)}")
                load()
                revoke = null
            }
        }
    }
    if (logout)
        Confirm(
            "Se déconnecter ?",
            "La session sera fermée sur ce téléphone. Les tâches continueront sur le serveur.",
            state.busy,
            state.error,
            { logout = false },
        ) {
            vm.perform { logout() }
        }
}

@Composable
private fun TokenEditor(
    vm: LeoViewModel,
    state: Workspace,
    close: () -> Unit,
    refresh: suspend () -> Unit,
) {
    var label by rememberSaveable { mutableStateOf("") }
    var scopes by rememberSaveable { mutableStateOf(listOf("read")) }
    // Displayed once, never stored in SavedState, files, or a ViewModel.
    var token by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = { if (!state.busy) close() },
        properties = DialogProperties(securePolicy = SecureFlagPolicy.SecureOn),
        title = { Text(if (token.isEmpty()) "Créer un jeton" else "Votre nouveau jeton") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (token.isEmpty()) {
                    Field("Nom du client", label, { label = it })
                    listOf(
                            "read" to "Lire",
                            "run" to "Lancer et arrêter des tâches",
                            "manage" to "Gérer les ressources",
                        )
                        .forEach { (scope, title) ->
                            Toggle(title, scope in scopes) { checked ->
                                scopes = if (checked) scopes + scope else scopes - scope
                            }
                        }
                } else {
                    Text("Copiez ce jeton maintenant. Il ne sera plus affiché après fermeture.")
                    Code(token)
                    CopyButton("Copier le jeton", token, sensitive = true)
                }
                state.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            if (token.isEmpty())
                TextButton(
                    onClick = {
                        vm.perform {
                            token =
                                api.send<TokenResult>(
                                        "POST",
                                        "/tokens",
                                        buildJsonObject {
                                            put("label", label)
                                            put("scopes", wireJson.encodeToJsonElement(scopes))
                                        },
                                    )
                                    .token
                            refresh()
                        }
                    },
                    enabled =
                        label.isNotBlank() &&
                            label.length <= 100 &&
                            scopes.isNotEmpty() &&
                            !state.busy,
                ) {
                    Text("Créer")
                }
            else TextButton(onClick = close) { Text("Terminé") }
        },
        dismissButton = {
            if (token.isEmpty())
                TextButton(onClick = close, enabled = !state.busy) { Text("Annuler") }
        },
    )
}
