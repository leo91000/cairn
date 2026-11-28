package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue

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

    fun avatarLoaderAnimatesOnlyWhileWorking() {
        var item by
            mutableStateOf(
                FeedItem(
                    "chat",
                    FeedKind.RUNNING_CHAT,
                    "Work",
                    "",
                    "Agent",
                    "agent",
                    working = true,
                    statusLabel = "En cours",
                )
            )
        compose.mainClock.autoAdvance = false
        compose.setContent { CairnTheme { FilAvatar(item, 28.dp) } }
        compose.mainClock.advanceTimeByFrame()
        compose.onNodeWithContentDescription(item.statusLabel).assertIsDisplayed()
        val first = markPixels()
        compose.mainClock.advanceTimeBy(256)
        assertFalse("The working badge must visibly rotate", first.contentEquals(markPixels()))

        compose.runOnIdle { item = item.copy(working = false, statusLabel = "En attente") }
        compose.mainClock.advanceTimeByFrame()
        val queued = markPixels()
        compose.mainClock.advanceTimeBy(256)
        assertTrue("Queued work must stay still", queued.contentEquals(markPixels()))

        compose.mainClock.autoAdvance = true
        compose.runOnIdle {
            item = item.copy(kind = FeedKind.UNREAD_CHAT, finished = true, statusLabel = "Terminé")
        }
        compose.onNodeWithTag("fil-mark").assertDoesNotExist()
    }

    fun conversationReadCheck() {
        var item by
            mutableStateOf(
                FeedItem(
                    key = "chat:result",
                    kind = FeedKind.UNREAD_CHAT,
                    title = "Résultat à lire",
                    subtitle = "",
                    agent = "Agent",
                    agentKey = "agent",
                    chatId = "result",
                    readAt = 1000,
                )
            )
        var reads = 0
        var snoozes = 0
        compose.setContent {
            CairnTheme {
                FilRowActions(
                    item,
                    markRead = {
                        reads++
                        item = item.copy(unread = false)
                    },
                    snooze = { snoozes++ },
                    restore = {},
                )
            }
        }
        compose.onNodeWithContentDescription("Autres actions pour Résultat à lire").performClick()
        compose.onNodeWithText("Marquer comme lu").performClick()
        compose.runOnIdle { assertEquals(1, reads) }
        compose.onNodeWithContentDescription("Autres actions pour Résultat à lire").performClick()
        compose.onNodeWithText("Marquer comme lu").assertDoesNotExist()
        compose.onNodeWithText("Reporter jusqu’à…").performClick()
        compose.runOnIdle { assertEquals(1, snoozes) }
    }

    private fun markPixels(): IntArray {
        val bitmap = compose.onNodeWithTag("fil-mark").captureToImage().asAndroidBitmap()
        return IntArray(bitmap.width * bitmap.height).also {
            bitmap.getPixels(it, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
        }
    }
}
