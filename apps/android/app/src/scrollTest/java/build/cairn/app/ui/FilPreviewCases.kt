package build.cairn.app.ui

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.Density
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

abstract class FilPreviewCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun longPreviewKeepsOpenReachableWithLargeText() {
        var opened = false
        val item =
            FeedItem(
                key = "chat:preview",
                kind = FeedKind.UNREAD_CHAT,
                title = "Résultat détaillé de la vérification et des corrections. ".repeat(8),
                subtitle = "Terminée",
                agent = "Reviewer",
                agentKey = "reviewer",
                finished = true,
                preview =
                    (1..20).joinToString("\n") { "Point $it : les changements ont été vérifiés." },
            )
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 1.8f)) {
                CairnTheme("dark") {
                    PeekSheet(item, now = 0, close = {}, open = { opened = true })
                }
            }
        }
        compose.onNodeWithText("Ouvrir").performScrollTo().assertIsDisplayed().performClick()
        compose.runOnIdle { assertTrue(opened) }
    }
}
