package build.cairn.app.ui

import android.app.Application
import androidx.compose.runtime.*
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/** The real native sheet consumes the same normalized payload as the browser. */
abstract class OpenCodeGoUsageCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun threeQuotasAndPaidBalanceRemainDistinct() {
        val initial =
            wireJson.decodeFromString<Account>(
                """{"id":"go","provider":"opencode","name":"Go usage","modelProvider":"opencode-go","modelProviderName":"OpenCode Go","state":"ready","status":"next","stale":false,"remainingPercent":15,"usage":{"useBalance":false,"windows":[{"id":"go:rolling","label":"5-hour window","usedPercent":25,"durationMins":300,"resetsAt":1893499200},{"id":"go:weekly","label":"Weekly","usedPercent":60,"durationMins":10080,"resetsAt":1894017600},{"id":"go:monthly","label":"Monthly","usedPercent":85,"durationMins":43200,"resetsAt":1896084000}]}}"""
            )
        var account by mutableStateOf(initial)
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    override fun read(origin: String): String? = null

                    override fun write(origin: String, cookie: String?) = Unit
                },
                beaconOrigin = "",
            )
        compose.setContent {
            CairnTheme {
                AccountSheet(vm, Workspace(), account, {}, {}, {}, {})
            }
        }
        compose.onNodeWithText("5 heures · 75 %").assertExists()
        compose.onNodeWithText("Semaine · 40 %").assertExists()
        compose.onNodeWithText("Mois · 15 %").assertExists()
        compose.onAllNodesWithText("Reprise ", substring = true).assertCountEquals(3)
        compose.onAllNodesWithText("crédits Zen payants", substring = true).assertCountEquals(0)
        compose.runOnIdle {
            val usage = initial.usage!!
            account =
                initial.copy(
                    remainingPercent = 0.0,
                    usage =
                        usage.copy(
                            useBalance = true,
                            windows =
                                usage.windows.map {
                                    if (it.id == "go:monthly") it.copy(usedPercent = 100.0) else it
                                },
                        ),
                )
        }
        compose.onNodeWithText("Mois · 0 %").assertExists()
        compose
            .onNodeWithText("crédits Zen payants", substring = true)
            .performScrollTo()
            .assertIsDisplayed()
        assertTrue(
            wireJson
                .decodeFromString<Account>(wireJson.encodeToString(account))
                .usage!!
                .useBalance == true
        )
        assertFalse(
            wireJson
                .decodeFromString<Account>("""{"id":"old","name":"Existing"}""")
                .usage
                ?.useBalance ?: false
        )
    }
}
