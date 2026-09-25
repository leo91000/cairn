package dev.leo.manager.ui

import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType

/** Keyboard hints only: the IME remains responsible for casing and correction. */
internal object InputKeyboards {
    val Sentences =
        KeyboardOptions(
            capitalization = KeyboardCapitalization.Sentences,
            autoCorrectEnabled = true,
        )

    // Preserve literal input for paths, identifiers, source files and configuration.
    val Literal =
        KeyboardOptions(
            capitalization = KeyboardCapitalization.None,
            autoCorrectEnabled = false,
        )
    val Uri = Literal.copy(keyboardType = KeyboardType.Uri)
    val Number = Literal.copy(keyboardType = KeyboardType.Number)
    val Password = Literal.copy(keyboardType = KeyboardType.Password)
    val Search = Literal.copy(imeAction = ImeAction.Search)
}
