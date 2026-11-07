package build.cairn.app.ui

import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.ChatPlanSettings
import build.cairn.contracts.GetChatPlanSettingsInput
import build.cairn.contracts.getChatPlanSettings
import build.cairn.contracts.saveChatPlanSettings
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@Composable
internal fun ChatPlanSettings(vm: CairnViewModel) {
    ChatPlanSettings(
        load = { vm.api.getChatPlanSettings(GetChatPlanSettingsInput()) },
        save = { vm.api.saveChatPlanSettings(it) },
    )
}

@Composable
internal fun ChatPlanSettings(
    load: suspend () -> ChatPlanSettings,
    save: suspend (ChatPlanSettings) -> ChatPlanSettings,
) {
    var settings by remember { mutableStateOf<ChatPlanSettings?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    suspend fun reload() {
        busy = true
        error = null
        try {
            settings = load()
        } catch (e: Exception) {
            if (e is CancellationException) throw e
            error = e.message ?: "Impossible de charger les paramètres."
        } finally {
            busy = false
        }
    }
    LaunchedEffect(Unit) { reload() }
    Panel {
        Text("Mode Plan du chat", style = MaterialTheme.typography.titleLarge)
        Text(
            "Autorise Plan dans le composeur pour cette installation. Désactivé par défaut. Avec Codex ou Claude Code, Plan explore et propose une approche avant votre choix de l’implémenter. Les demandes de plan déjà acceptées gardent leurs restrictions après désactivation."
        )
        settings?.let { current ->
            Text(if (current.enabled) "Activé" else "Désactivé")
            Switch(
                modifier =
                    Modifier.semantics {
                        contentDescription = "Autoriser le mode Plan dans le chat"
                    },
                checked = current.enabled,
                enabled = !busy,
                onCheckedChange = { enabled ->
                    scope.launch {
                        busy = true
                        error = null
                        try {
                            settings = save(current.copy(enabled = enabled))
                        } catch (e: Exception) {
                            if (e is CancellationException) throw e
                            error = e.message ?: "Impossible d’enregistrer les paramètres."
                        } finally {
                            busy = false
                        }
                    }
                },
            )
        }
        if (busy) LinearProgressIndicator()
        error?.let {
            Text(it, color = MaterialTheme.colorScheme.error)
            OutlinedButton(enabled = !busy, onClick = { scope.launch { reload() } }) {
                Text("Recharger les paramètres du mode Plan")
            }
        }
    }
}
