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
 * so; [save] receives the trimmed title only when it changed.
 */
@Composable
internal fun ConversationRename(
    title: String,
    busy: Boolean,
    error: String?,
    dismiss: () -> Unit,
    save: (String) -> Unit,
) {
    var value by remember { mutableStateOf(TextFieldValue(title, TextRange(0, title.length))) }
    val trimmed = value.text.trim()
    // Counted in characters like the installation, so emoji and longer titles stay editable.
    val length = trimmed.codePointCount(0, trimmed.length)
    val tooLong = length > MAX_CONVERSATION_TITLE
    val changed = trimmed.isNotEmpty() && !tooLong && trimmed != title
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
                    // Typing stops at the limit; deleting, moving the cursor or selecting never
                    // does.
                    onValueChange = {
                        val next = it.text.codePointCount(0, it.text.length)
                        if (
                            next <= MAX_CONVERSATION_TITLE ||
                                next <= value.text.codePointCount(0, value.text.length)
                        )
                            value = it
                    },
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
                            onDone = { if (changed && !busy) save(trimmed) }
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
                if (error != null) Text(error, color = MaterialTheme.colorScheme.error)
            }
        },
        confirmButton = {
            TextButton(onClick = { save(trimmed) }, enabled = changed && !busy) {
                Text("Enregistrer")
            }
        },
        dismissButton = { TextButton(onClick = dismiss, enabled = !busy) { Text("Annuler") } },
    )
}
