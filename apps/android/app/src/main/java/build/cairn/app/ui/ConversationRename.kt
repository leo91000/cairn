package build.cairn.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SheetValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
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
 * Renames a conversation whose current title is [title], in a sheet like the web's phone layout.
 * Once renamed, Cairn no longer names it automatically, so the sheet says so. [save] receives the
 * trimmed title only when it changed, the conversation's title as the sheet last showed it, and
 * where to report a different title found when saving.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ConversationRename(
    title: String,
    busy: Boolean,
    error: String?,
    dismiss: () -> Unit,
    save: (title: String, current: String, found: (String) -> Unit) -> Unit,
) {
    // The title the sheet opened on. Nothing that happens meanwhile blocks the rename, but a title
    // that changed (automatically or elsewhere) is never replaced unseen: the sheet names it first.
    val shown = remember { title }
    // A newer title found when saving, until the live conversation brings its own.
    var found by remember(title) { mutableStateOf<String?>(null) }
    val current = found ?: title
    val meanwhile = current.takeIf { it != shown }
    var value by remember { mutableStateOf(TextFieldValue(shown, TextRange(0, shown.length))) }
    val trimmed = value.text.trim()
    // Counted in characters like the installation, so emoji and longer titles stay editable.
    val length = trimmed.codePointCount(0, trimmed.length)
    val tooLong = length > MAX_CONVERSATION_TITLE
    // Compared with the conversation's current title, which the sheet names when it changed.
    val changed = trimmed.isNotEmpty() && !tooLong && trimmed != current
    // The shared error belongs to this sheet only once it has tried to save.
    var tried by remember { mutableStateOf(false) }
    val savable = changed && !busy
    val submit = {
        tried = true
        save(trimmed, current) { found = it }
    }
    val focus = remember { FocusRequester() }
    // A sheet hides before it reports its dismissal, so it stays up while a save is in flight.
    val saving by rememberUpdatedState(busy)
    ModalBottomSheet(
        onDismissRequest = dismiss,
        sheetState =
            rememberModalBottomSheetState(
                skipPartiallyExpanded = true,
                confirmValueChange = { it != SheetValue.Hidden || !saving },
            ),
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        // The field exists once the sheet's content composes; a window not yet attached is simply
        // left unfocused.
        LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 20.dp).padding(bottom = 28.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Renommer la conversation", style = MaterialTheme.typography.headlineSmall)
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
                keyboardActions = KeyboardActions(onDone = { if (savable) submit() }),
                modifier = Modifier.fillMaxWidth().focusRequester(focus).testTag("rename-title"),
            )
            Text(
                "Cairn ne renommera plus cette conversation automatiquement.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            ConnectionFeedbackNotice()
            if (meanwhile != null)
                Text(
                    "Le titre est devenu « $meanwhile » entre-temps ; enregistrer le remplace.",
                    color = MaterialTheme.colorScheme.error,
                )
            if (tried && error != null) Text(error, color = MaterialTheme.colorScheme.error)
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.End),
            ) {
                TextButton(onClick = dismiss, enabled = !busy) { Text("Annuler") }
                Button(onClick = submit, enabled = savable) { Text("Enregistrer") }
            }
        }
    }
}
