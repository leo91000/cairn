package build.cairn.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.contracts.ChatPlanSettings
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

abstract class ChatPlanSettingsCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun installationPlanSettingLoadsDisabledAndSavesBothChoicesAtTheirRevision() {
        val saves = CopyOnWriteArrayList<ChatPlanSettings>()
        compose.setContent {
            MaterialTheme {
                Page {
                    ChatPlanSettings(
                        load = { ChatPlanSettings(enabled = false, revision = "initial") },
                        save = { input ->
                            saves += input
                            input.copy(revision = "saved-${saves.size}")
                        },
                    )
                }
            }
        }
        val toggle = compose.onNodeWithContentDescription("Autoriser le mode Plan dans le chat")
        toggle.performScrollTo().assertIsOff().performClick()
        compose.waitUntil { saves.size == 1 }
        toggle.assertIsOn()
        assertEquals(ChatPlanSettings(enabled = true, revision = "initial"), saves[0])
        toggle.performClick()
        compose.waitUntil { saves.size == 2 }
        toggle.assertIsOff()
        assertEquals(ChatPlanSettings(enabled = false, revision = "saved-1"), saves[1])
    }

    @Test
    fun failedSaveKeepsTheSavedStateAndReloadsTheRevision() {
        var loads = 0
        val saves = CopyOnWriteArrayList<ChatPlanSettings>()
        compose.setContent {
            MaterialTheme {
                Page {
                    ChatPlanSettings(
                        load = {
                            loads++
                            ChatPlanSettings(enabled = false, revision = "load-$loads")
                        },
                        save = { input ->
                            saves += input
                            if (saves.size == 1) error("Révision périmée")
                            input.copy(revision = "saved")
                        },
                    )
                }
            }
        }
        val toggle = compose.onNodeWithContentDescription("Autoriser le mode Plan dans le chat")
        toggle.performScrollTo().performClick()
        compose.onNodeWithText("Révision périmée").assertExists()
        toggle.assertIsOff()
        compose
            .onNodeWithText("Recharger les paramètres du mode Plan")
            .performScrollTo()
            .performClick()
        compose.waitUntil { loads == 2 }
        toggle.performScrollTo().performClick()
        compose.waitUntil { saves.size == 2 }
        assertEquals("load-2", saves[1].revision)
        toggle.assertIsOn()
    }
}
