package build.cairn.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.contracts.CodingAgentConfiguration
import build.cairn.contracts.CodingAgentSetting
import build.cairn.contracts.EditCodingAgentConfigurationInput
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.CompletableDeferred
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

abstract class CodingAgentConfigurationCases {
    @get:Rule val compose = createComposeRule()

    protected open fun capture(name: String) = Unit

    private val initial =
        CodingAgentConfiguration(
            agentId = null,
            agentName = null,
            customizedPaths = emptyList(),
            document = "model_context_window = 200000\nmodel_auto_compact_token_limit = 180000\n",
            filename = "config.toml",
            inheritedDocument = "",
            managed = emptyList(),
            notices = emptyList(),
            provider = "codex",
            revision = "fixture-revision",
            settings =
                listOf(
                    setting("/model_context_window", "Context window", "200000"),
                    setting(
                        "/model_auto_compact_token_limit",
                        "Automatic compaction threshold",
                        "180000",
                    ),
                ),
        )

    private fun setting(path: String, label: String, value: String) =
        CodingAgentSetting(
            choices = emptyList(),
            description = "Native tokens",
            group = "Context",
            inherited = true,
            kind = "integer",
            label = label,
            path = path,
            value = value,
        )

    private fun preview(input: EditCodingAgentConfigurationInput): CodingAgentConfiguration {
        if (input.changes.isEmpty()) return initial.copy(document = input.document)
        val values = initial.settings.associate { it.path to it.value }.toMutableMap()
        for (change in input.changes) values[change.path] = change.value
        return initial.copy(
            document =
                values.entries.joinToString("\n", postfix = "\n") {
                    "${it.key.removePrefix("/")} = ${it.value}"
                },
            settings = initial.settings.map { it.copy(value = values[it.path]) },
        )
    }

    @Test
    fun rapidChangesKeepEveryPendingFieldAndSaveOneNativeDocument() {
        val requests = CopyOnWriteArrayList<EditCodingAgentConfigurationInput>()
        val saves = CopyOnWriteArrayList<EditCodingAgentConfigurationInput>()
        val release = CompletableDeferred<Unit>()
        compose.setContent {
            MaterialTheme {
                CodingAgentConfigurationEditor(
                    "codex",
                    {},
                    { initial },
                    {
                        requests.add(it)
                        release.await()
                        preview(it)
                    },
                    {
                        saves.add(it)
                        preview(it)
                    },
                )
            }
        }
        compose.onNodeWithText("Fenêtre de contexte").performTextReplacement("1000000")
        compose.waitUntil { requests.isNotEmpty() }
        compose.onNodeWithText("Seuil de compaction automatique").performTextReplacement("800000")
        compose.waitUntil { requests.last().changes.size == 2 }
        assertEquals(
            "1000000",
            requests.last().changes.first { it.path == "/model_context_window" }.value,
        )
        assertEquals(
            "800000",
            requests.last().changes.first { it.path == "/model_auto_compact_token_limit" }.value,
        )
        release.complete(Unit)
        compose.waitForIdle()
        capture("form")
        compose.onNodeWithText("Enregistrer", substring = false).assertIsEnabled().performClick()
        compose.waitUntil { saves.isNotEmpty() }
        assertTrue(saves.single().document.contains("model_context_window = 1000000"))
        assertTrue(saves.single().document.contains("model_auto_compact_token_limit = 800000"))
    }

    @Test
    fun rawDraftSurvivesStateRestorationAndCancellingNavigation() {
        var loads = 0
        var backs = 0
        val restoration = StateRestorationTester(compose)
        restoration.setContent {
            MaterialTheme {
                CodingAgentConfigurationEditor(
                    "codex",
                    { backs++ },
                    {
                        loads++
                        initial
                    },
                    { preview(it) },
                    { preview(it) },
                )
            }
        }
        compose.onNodeWithText("config.toml", substring = false).performClick()
        val draft = "# Keep my context preference\nmodel_context_window = 1000000\n"
        compose.onNodeWithText("Configuration native").performTextReplacement(draft)
        compose.waitForIdle()
        restoration.emulateSavedInstanceStateRestore()
        compose.onNodeWithText("Configuration native").assertTextContains(draft)
        assertEquals(1, loads)
        compose.onNodeWithContentDescription("Retour").performClick()
        compose.onNodeWithText("Continuer à modifier").performClick()
        compose.onNodeWithText("Configuration native").assertTextContains(draft)
        assertEquals(0, backs)
        capture("text")
    }

    @Test
    fun resetAllClearsInvalidFieldsAndSavesTheReplacementDocument() {
        val configured =
            initial.copy(
                agentId = "00000000-0000-4000-8000-000000000001",
                inheritedDocument = initial.document,
                customizedPaths = listOf("/model_context_window"),
            )
        val reset = configured.copy(customizedPaths = emptyList())
        val previews = CopyOnWriteArrayList<EditCodingAgentConfigurationInput>()
        val saves = CopyOnWriteArrayList<EditCodingAgentConfigurationInput>()
        val hold = CompletableDeferred<Unit>()
        compose.setContent {
            MaterialTheme {
                CodingAgentConfigurationEditor(
                    "codex",
                    {},
                    { configured },
                    { input ->
                        previews.add(input)
                        if (input.reset) reset
                        else {
                            hold.await()
                            preview(input)
                        }
                    },
                    { input ->
                        saves.add(input)
                        reset
                    },
                )
            }
        }
        compose.onNodeWithText("Seuil de compaction automatique").performTextReplacement("800000")
        compose.waitUntil { previews.isNotEmpty() }
        compose.onNodeWithText("Fenêtre de contexte").performTextReplacement("0")
        compose.onNodeWithText("Fenêtre de contexte : saisissez un entier positif.").assertExists()
        compose
            .onNodeWithText("Réinitialiser les personnalisations")
            .performScrollTo()
            .performClick()
        compose.waitForIdle()
        compose
            .onNodeWithText("Fenêtre de contexte : saisissez un entier positif.")
            .assertDoesNotExist()
        compose.onNodeWithText("Fenêtre de contexte").assertTextContains("200000")
        compose.onNodeWithText("Seuil de compaction automatique").assertTextContains("180000")
        compose.onNodeWithText("Enregistrer", substring = false).assertIsEnabled().performClick()
        compose.waitUntil { saves.isNotEmpty() }
        assertEquals(initial.document, saves.single().document)
        assertTrue(previews.last().reset)
        assertTrue(previews.last().changes.isEmpty())
        assertTrue(saves.single().inheritPaths.contains("/model_context_window"))
    }

    @Test
    fun invalidNumberCanBeCorrectedWithoutLeavingTheForm() {
        compose.setContent {
            MaterialTheme {
                CodingAgentConfigurationEditor(
                    "codex",
                    {},
                    { initial },
                    { preview(it) },
                    { preview(it) },
                )
            }
        }
        val field = compose.onNodeWithText("Fenêtre de contexte")
        field.performTextReplacement("0")
        compose.onNodeWithText("Fenêtre de contexte : saisissez un entier positif.").assertExists()
        field.assertIsEnabled().performTextReplacement("1000000")
        compose.waitForIdle()
        compose.onNodeWithText("Enregistrer", substring = false).assertIsEnabled()
    }
}
