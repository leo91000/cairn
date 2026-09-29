package dev.leo.manager.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.unit.dp
import dev.leo.manager.data.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ModelPickerTest : ModelPickerCases() {
    override fun captureReasoning() = capture("reasoning")

    private fun capture(name: String, tag: String = "model-settings-sheet") {
        val file = java.io.File("build/reports/model-picker/$name.png")
        file.parentFile!!.mkdirs()
        compose.onNodeWithTag(tag).captureToImage().asAndroidBitmap().let { bitmap ->
            file.outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }

    @Test
    fun unifiedSheetGroupsAgentModelAndReasoning() {
        val efforts = listOf("low", "medium", "high", "xhigh").map { ReasoningOption(it) }
        val codex =
            ModelCatalog(
                listOf(
                    CodexModel(
                        "gpt-6-sol",
                        "GPT-6 Sol",
                        "Le plus capable pour le code complexe.",
                        isDefault = true,
                        defaultReasoningEffort = "medium",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodexModel(
                        "gpt-6-luna",
                        "GPT-6 Luna",
                        "Rapide et économique pour les tâches courantes.",
                        defaultReasoningEffort = "low",
                        supportedReasoningEfforts = efforts,
                    ),
                )
            )
        val claude =
            ModelCatalog(
                listOf(
                    CodexModel(
                        "default",
                        "Par défaut",
                        "Modèle recommandé pour votre abonnement.",
                        isDefault = true,
                        defaultReasoningEffort = "high",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodexModel(
                        "opus",
                        "Opus",
                        "Pour les travaux les plus exigeants.",
                        defaultReasoningEffort = "high",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodexModel(
                        "sonnet",
                        "Sonnet",
                        "Équilibre entre vitesse et qualité.",
                        defaultReasoningEffort = "medium",
                        supportedReasoningEfforts = efforts,
                    ),
                )
            )
        var theme by mutableStateOf("light")
        compose.setContent {
            var provider by remember { mutableStateOf("codex") }
            var model by remember { mutableStateOf("gpt-6-sol") }
            var reasoning by remember { mutableStateOf("high") }
            LeoTheme(theme) {
                Surface(Modifier.fillMaxSize()) {
                    Column(Modifier.padding(12.dp).testTag("picker-preview")) {
                        ModelPicker(
                            if (provider == "claude") claude else codex,
                            model,
                            reasoning,
                            "gpt-6-sol",
                            inherit = true,
                            provider = provider,
                            changeProvider = {
                                provider = it
                                model = ""
                                reasoning = ""
                            },
                            switching = provider == "claude",
                        ) { m, r ->
                            model = m
                            reasoning = r
                        }
                    }
                }
            }
        }
        capture("assistant-pill-light", "picker-preview")
        compose.onNodeWithTag("model-picker").performClick()
        capture("assistant-codex-light")
        compose.onNodeWithText("Claude Code").performClick()
        compose.onNodeWithText("Opus").performClick()
        capture("assistant-claude-light")
        theme = "dark"
        compose.waitForIdle()
        capture("assistant-claude-dark")
        compose.onNodeWithText("Codex").performClick()
        capture("assistant-codex-dark")
        compose.onNodeWithText("Terminé").performClick()
        capture("assistant-pill-dark", "picker-preview")
    }
}
