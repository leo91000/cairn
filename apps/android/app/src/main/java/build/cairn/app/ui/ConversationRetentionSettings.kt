package build.cairn.app.ui

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.text.input.KeyboardType
import build.cairn.app.data.CairnViewModel
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

@Serializable private data class ConversationRetention(val inactivityDays: Int)

@Composable
internal fun ConversationRetentionSettings(vm: CairnViewModel) {
    ConversationRetentionSettings(
        load = { vm.api.get<ConversationRetention>("/settings/conversations").inactivityDays },
        save = { days ->
            vm.api.send<ConversationRetention>(
                "PUT",
                "/settings/conversations",
                buildJsonObject { put("inactivityDays", days) },
            )
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
            "Suppression automatique des conversations",
            style = MaterialTheme.typography.titleLarge,
        )
        Text(
            "Supprime définitivement les conversations après ce délai sans activité, y compris celles déjà présentes dans cette installation. L’historique, les fichiers et les liens partagés sont supprimés. Indiquez 0 pour désactiver."
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
                "La suppression automatique sera désactivée. Les conversations déjà supprimées ne peuvent pas être récupérées."
            else
                "Les conversations inactives depuis $days jours seront définitivement supprimées, y compris celles déjà présentes. Cette action est irréversible.",
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
