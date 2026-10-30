package build.cairn.app.ui

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.text.input.KeyboardType
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.ConversationRetention
import build.cairn.contracts.GetConversationRetentionInput
import build.cairn.contracts.getConversationRetention
import build.cairn.contracts.setConversationRetention
import kotlinx.coroutines.launch

@Composable
internal fun ConversationRetentionSettings(vm: CairnViewModel) {
    ConversationRetentionSettings(
        load = {
            vm.api.getConversationRetention(GetConversationRetentionInput()).inactivityDays
        },
        save = { days ->
            vm.api.setConversationRetention(ConversationRetention(inactivityDays = days))
        },
    )
}

@Composable
internal fun ConversationRetentionSettings(load: suspend () -> Int, save: suspend (Int) -> Unit) {
    var days by rememberSaveable { mutableStateOf("7") }
    var loaded by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var confirming by rememberSaveable { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var saved by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val valid = days.toIntOrNull()?.let { it in 0..36500 } == true
    LaunchedEffect(Unit) {
        try {
            days = load().toString()
            loaded = true
        } catch (e: Exception) {
            error = e.message
        }
    }
    Panel {
        Text(
            "Nettoyage automatique des environnements",
            style = MaterialTheme.typography.titleLarge,
        )
        Text(
            "Supprime les environnements de travail après ce délai sans activité, y compris pour les conversations existantes. L’historique, les pièces jointes, toutes les versions des livrables et les liens partagés sont conservés. Les modifications non publiées, outils stockés dans l’environnement et autres fichiers de travail sont définitivement supprimés. Le prochain message crée un nouvel environnement et en informe l’agent. Indiquez 0 pour désactiver."
        )
        Text(
            "Les agents actifs, messages en attente, questions ouvertes et réveils programmés sont protégés. Le délai part de la dernière activité ou de la fin du travail de l’agent. Lire une conversation ne relance pas le délai."
        )
        if (loaded) {
            Field(
                "Jours d’inactivité",
                days,
                {
                    days = it
                    saved = false
                },
                enabled = !busy,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            )
            Button(
                onClick = {
                    error = null
                    confirming = true
                },
                enabled = !busy && valid,
            ) {
                Text("Enregistrer le délai")
            }
        } else if (error == null) LinearProgressIndicator()
        if (!confirming) error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
        if (saved) Text("Délai enregistré")
    }
    if (confirming) {
        Confirm(
            "Enregistrer le délai ?",
            if (days.toIntOrNull() == 0)
                "Le nettoyage automatique sera désactivé. Les fichiers de travail déjà supprimés ne peuvent pas être récupérés."
            else
                "Les environnements inactifs depuis $days jours seront définitivement supprimés, y compris pour les conversations existantes. L’historique, les pièces jointes et les livrables sont conservés. Les modifications non publiées ne pourront pas être récupérées.",
            busy,
            error,
            { confirming = false },
        ) {
            scope.launch {
                busy = true
                error = null
                try {
                    save(days.toInt())
                    confirming = false
                    saved = true
                } catch (e: Exception) {
                    error = e.message
                } finally {
                    busy = false
                }
            }
        }
    }
}
