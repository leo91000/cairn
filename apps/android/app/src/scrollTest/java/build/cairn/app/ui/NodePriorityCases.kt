package build.cairn.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

abstract class NodePriorityCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun dragMovesAcrossRowsInBothDirectionsAndCancelKeepsTheDraft() {
        var order by mutableStateOf(listOf("Alpha", "Beta", "Gamma"))
        compose.setContent {
            MaterialTheme {
                Column {
                    NodePriorityList(order, { it }, true, { order = it }, { order = order - it })
                }
            }
        }
        drag("Gamma", "Alpha")
        compose.runOnIdle { assertEquals(listOf("Gamma", "Alpha", "Beta"), order) }
        compose.onNodeWithText("1. Gamma").assertExists()
        drag("Gamma", "Beta")
        compose.runOnIdle { assertEquals(listOf("Alpha", "Beta", "Gamma"), order) }
        drag("Gamma", "Alpha", cancel = true)
        compose.runOnIdle { assertEquals(listOf("Alpha", "Beta", "Gamma"), order) }
        compose.onNodeWithContentDescription("Monter Beta").performClick()
        compose.runOnIdle { assertEquals(listOf("Beta", "Alpha", "Gamma"), order) }
    }

    @Test
    fun unavailableNodeDataKeepsPrioritiesReadOnly() {
        var changes = 0
        compose.setContent {
            MaterialTheme {
                Column {
                    NodePriorityList(listOf("Alpha", "Beta"), { it }, false, { changes++ }, {})
                }
            }
        }
        drag("Alpha", "Beta")
        compose.runOnIdle { assertEquals(0, changes) }
        compose.onNodeWithText("1. Alpha").assertExists()
        compose.onNodeWithContentDescription("Descendre Alpha").assertIsNotEnabled()
    }

    private fun drag(from: String, to: String, cancel: Boolean = false) {
        val source = compose.onNodeWithContentDescription("Déplacer $from")
        val destination = compose.onNodeWithContentDescription("Déplacer $to")
        val delta =
            destination.fetchSemanticsNode().boundsInRoot.center.y -
                source.fetchSemanticsNode().boundsInRoot.center.y
        source.performTouchInput {
            if (cancel) {
                down(center)
                moveBy(Offset(0f, delta / 2))
                moveBy(Offset(0f, delta / 2))
                cancel()
            } else {
                swipe(center, center + Offset(0f, delta), durationMillis = 500)
            }
        }
        compose.waitForIdle()
    }
}
