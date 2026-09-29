package dev.leo.manager.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.widget.Toast
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
internal fun ArtifactSharingDialog(vm: LeoViewModel, artifact: Deliverable, close: () -> Unit) {
    var current by remember(artifact.id) { mutableStateOf<Deliverable?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val path = artifact.path().substringBefore('?')
    suspend fun load(visibility: String? = null) {
        busy = true
        error = null
        try {
            current =
                if (visibility == null) vm.api.get<Deliverable>("$path?metadata=1")
                else
                    wireJson.decodeFromString<Deliverable>(
                        vm.api.request(
                            "PUT",
                            "$path/visibility",
                            buildJsonObject { put("visibility", visibility) },
                        )
                    )
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            error = e.message ?: "Impossible de modifier le partage."
        } finally {
            busy = false
        }
    }
    LaunchedEffect(artifact.id) { load() }
    val public = current?.visibility == "public" && current?.publicUrl != null
    AlertDialog(
        onDismissRequest = close,
        title = { Text("Partage du fichier") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    if (public) "Lien public activé" else "Fichier privé",
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    "Toute personne disposant du lien peut lire cette version sans connexion. Les autres fichiers et versions restent privés."
                )
                if (busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (public) {
                    val url = current!!.publicUrl!!
                    SelectionContainer { Text(url, style = MaterialTheme.typography.bodySmall) }
                    Column {
                        TextButton(
                            enabled = !busy,
                            onClick = {
                                context
                                    .getSystemService(ClipboardManager::class.java)
                                    .setPrimaryClip(ClipData.newPlainText("Lien public", url))
                                Toast.makeText(context, "Lien copié", Toast.LENGTH_SHORT).show()
                            },
                        ) {
                            Text("Copier le lien")
                        }
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
                    }
                    Text(
                        "Désactiver le lien bloque les prochains accès, pas les copies déjà téléchargées.",
                        style = MaterialTheme.typography.bodySmall,
                    )
                }
                error?.let {
                    Text(it, color = MaterialTheme.colorScheme.error)
                    TextButton(enabled = !busy, onClick = { scope.launch { load() } }) {
                        Text("Réessayer")
                    }
                }
                OutlinedButton(
                    enabled = !busy && current != null,
                    onClick = { scope.launch { load(if (public) "private" else "public") } },
                ) {
                    Text(if (public) "Désactiver le lien public" else "Activer le lien public")
                }
            }
        },
        confirmButton = { TextButton(onClick = close) { Text("Fermer") } },
    )
}
