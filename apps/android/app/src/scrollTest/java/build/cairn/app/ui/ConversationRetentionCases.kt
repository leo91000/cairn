package build.cairn.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

abstract class ConversationRetentionCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun retentionLoadsConfirmsCustomDaysAndCanBeDisabled() {
        val saves = CopyOnWriteArrayList<Int>()
        compose.setContent {
            MaterialTheme {
                Page {
                    ConversationRetentionSettings(load = { 7 }, save = { saves += it })
                }
            }
        }
        compose.onNodeWithText("Jours d’inactivité").performScrollTo().assertTextContains("7")
        compose.onNodeWithText("Jours d’inactivité").performTextReplacement("14")
        compose.onNodeWithText("Enregistrer le délai").performScrollTo().performClick()
        compose.onNodeWithText("Enregistrer le délai ?").assertExists()
        assertEquals(emptyList<Int>(), saves.toList())
        compose.onNodeWithText("Annuler").performClick()
        assertEquals(emptyList<Int>(), saves.toList())
        compose.onNodeWithText("Enregistrer le délai").performClick()
        compose.onNodeWithText("Confirmer").performClick()
        compose.waitUntil { saves.size == 1 }
        assertEquals(listOf(14), saves.toList())
        compose.onNodeWithText("Délai enregistré").assertExists()
        compose.onNodeWithText("Jours d’inactivité").performTextReplacement("-1")
        compose.onNodeWithText("Enregistrer le délai").assertIsNotEnabled()
        compose.onNodeWithText("Jours d’inactivité").performTextReplacement("0")
        compose.onNodeWithText("Enregistrer le délai").performClick()
        compose.onNodeWithText("Confirmer").performClick()
        compose.waitUntil { saves.size == 2 }
        assertEquals(listOf(14, 0), saves.toList())
    }

    @Test
    fun failedSaveKeepsTheDialogOpenAndAllowsRetry() {
        var attempts = 0
        compose.setContent {
            MaterialTheme {
                Page {
                    ConversationRetentionSettings(
                        load = { 7 },
                        save = {
                            attempts++
                            if (attempts == 1) error("Enregistrement impossible")
                        },
                    )
                }
            }
        }
        compose.onNodeWithText("Enregistrer le délai").performScrollTo().performClick()
        compose.onNodeWithText("Confirmer").performClick()
        compose.onNodeWithText("Enregistrement impossible").assertExists()
        compose.onNodeWithText("Confirmer").performClick()
        compose.onNodeWithText("Délai enregistré").assertExists()
    }
}
