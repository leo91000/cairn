package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp

/** The installation keeps a title within 90 characters, trimmed and without control characters. */
internal const val MAX_CONVERSATION_TITLE = 90

/**
 * Renames a conversation. Once renamed, Cairn no longer names it automatically, so the dialog says
 * so; [save] receives the trimmed title only when it changed, and a way to report that the
 * conversation changed meanwhile, after which saving again waits for the dialog to reopen.
 */
@Composable
internal fun ConversationRename(
    title: String,
    busy: Boolean,
    error: String?,
    dismiss: () -> Unit,
    save: (title: String, conflict: () -> Unit) -> Unit,
) {
    var value by remember { mutableStateOf(TextFieldValue(title, TextRange(0, title.length))) }
    val trimmed = value.text.trim()
    // Counted in characters like the installation, so emoji and longer titles stay editable.
    val length = trimmed.codePointCount(0, trimmed.length)
    val tooLong = length > MAX_CONVERSATION_TITLE
    val changed = trimmed.isNotEmpty() && !tooLong && trimmed != title
    var stale by remember { mutableStateOf(false) }
    // The shared error belongs to this dialog only once it has tried to save.
    var tried by remember { mutableStateOf(false) }
    val savable = changed && !stale && !busy
    val submit = {
        tried = true
        save(trimmed) { stale = true }
    }
    val focus = remember { FocusRequester() }
    AlertDialog(
        onDismissRequest = { if (!busy) dismiss() },
        title = { Text("Renommer la conversation") },
        text = {
            // The field exists once the dialog's content composes; a window not yet attached is
            // simply left unfocused.
            LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
            Column {
                OutlinedTextField(
                    value = value,
                    // A longer title stays in the field with its length, like on the web.
                    onValueChange = { value = it },
                    isError = tooLong,
                    supportingText =
                        if (tooLong) {
                            {
                                Text(
                                    "$MAX_CONVERSATION_TITLE caractères au plus ($length actuellement)."
                                )
                            }
                        } else null,
                    label = { Text("Titre") },
                    singleLine = true,
                    enabled = !busy,
                    keyboardOptions =
                        KeyboardOptions(
                            capitalization = KeyboardCapitalization.Sentences,
                            imeAction = ImeAction.Done,
                        ),
                    keyboardActions =
                        androidx.compose.foundation.text.KeyboardActions(
                            onDone = { if (savable) submit() }
                        ),
                    modifier =
                        Modifier.fillMaxWidth().focusRequester(focus).testTag("rename-title"),
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    "Cairn ne renommera plus cette conversation automatiquement.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                ConnectionFeedbackNotice()
                if (tried && error != null) Text(error, color = MaterialTheme.colorScheme.error)
            }
        },
        confirmButton = {
            TextButton(onClick = submit, enabled = savable) {
                Text("Enregistrer")
            }
        },
        dismissButton = { TextButton(onClick = dismiss, enabled = !busy) { Text("Annuler") } },
    )
}
