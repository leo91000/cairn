package build.cairn.app.ui

import android.content.Intent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import build.cairn.app.data.CairnViewModel
import build.cairn.contracts.ChatInput
import build.cairn.contracts.ChatShare
import build.cairn.contracts.SetChatShareInput
import build.cairn.contracts.getChatShare
import build.cairn.contracts.revokeChatShare
import build.cairn.contracts.setChatShare
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@Composable
internal fun ConversationSharingDialog(vm: CairnViewModel, chatId: String, close: () -> Unit) {
    var current by remember(chatId) { mutableStateOf<ChatShare?>(null) }
    var hours by remember(chatId) { mutableStateOf("24") }
    var busy by remember { mutableStateOf(true) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    suspend fun update(action: String) {
        busy = true
        error = null
        try {
            current =
                when (action) {
                    "set" -> vm.api.setChatShare(SetChatShareInput(chatId, hours.toInt()))
                    "revoke" -> vm.api.revokeChatShare(ChatInput(chatId))
                    else -> vm.api.getChatShare(ChatInput(chatId))
                }
        } catch (cause: CancellationException) {
            throw cause
        } catch (cause: Exception) {
            error = cause.message ?: "Impossible de modifier le partage."
        } finally {
            busy = false
        }
    }
    LaunchedEffect(chatId) { update("get") }
    AlertDialog(
        onDismissRequest = close,
        title = { Text("Partager la conversation") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    "Toute personne disposant du lien peut lire la conversation et tous les fichiers publiés, y compris les prochains messages et fichiers jusqu’à expiration, sans connexion."
                )
                Text(
                    "La page demande aux moteurs de recherche de ne pas l’indexer. Réservez le lien à ses destinataires."
                )
                OutlinedTextField(
                    value = hours,
                    onValueChange = { hours = it },
                    label = { Text("Expiration en heures") },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    enabled = !busy,
                    supportingText = { Text("24 heures par défaut · De 1 heure à 30 jours") },
                )
                if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                current?.publicUrl?.let { url ->
                    current?.expiresAt?.let {
                        Text("Expire le ${DateFormat.getDateTimeInstance().format(Date(it))}")
                    }
                    SelectionContainer { Text(url, style = MaterialTheme.typography.bodySmall) }
                    TextButton(
                        enabled = !busy,
                        onClick = {
                            context.startActivity(
                                Intent.createChooser(
                                    Intent(Intent.ACTION_SEND).apply {
                                        type = "text/plain"
                                        putExtra(Intent.EXTRA_TEXT, url)
                                    },
                                    "Partager le lien",
                                )
                            )
                        },
                    ) {
                        Text("Partager le lien")
                    }
                    TextButton(enabled = !busy, onClick = { scope.launch { update("revoke") } }) {
                        Text("Révoquer le lien")
                    }
                    Text(
                        "Révoquer ou remplacer le lien bloque les prochains accès, pas les copies déjà téléchargées."
                    )
                }
                OutlinedButton(
                    enabled = !busy && current != null && hours.toIntOrNull() in 1..720,
                    onClick = { scope.launch { update("set") } },
                ) {
                    Text(
                        if (current?.publicUrl == null) "Créer le lien"
                        else "Remplacer avec cette expiration"
                    )
                }
                error?.let {
                    Text(it, color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !busy, onClick = { scope.launch { update("get") } }) {
                        Text("Réessayer")
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = close) { Text("Fermer") } },
    )
}
