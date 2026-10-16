package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.ComposeContentTestRule

internal class FilAttentionCases(private val compose: ComposeContentTestRule) {
    fun navigationBadge() {
        var attention by mutableStateOf(FilAttention(toRead = 2))
        compose.setContent {
            CairnTheme { CairnDock("missions", {}, {}, attention = attention) }
        }
        compose.onNodeWithContentDescription("2 à lire").assertIsDisplayed()
        compose.runOnIdle { attention = FilAttention(needsYou = 1, toRead = 2) }
        compose.onNodeWithContentDescription("1 pour vous · 2 à lire").assertIsDisplayed()
        compose.onNodeWithContentDescription("2 à lire").assertDoesNotExist()
        compose.runOnIdle { attention = FilAttention() }
        compose.onNodeWithContentDescription("1 pour vous · 2 à lire").assertDoesNotExist()
    }
}
