package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.*
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
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

    override fun captureDefaultModel() = capture("claude-default")

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
                    CodingAgentModel(
                        "gpt-6-sol",
                        "GPT-6 Sol",
                        "Le plus capable pour le code complexe.",
                        isDefault = true,
                        defaultReasoningEffort = "medium",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodingAgentModel(
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
                    CodingAgentModel(
                        "default",
                        "Par défaut",
                        "Modèle recommandé pour votre abonnement.",
                        isDefault = true,
                        defaultReasoningEffort = "high",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodingAgentModel(
                        "opus",
                        "Opus",
                        "Pour les travaux les plus exigeants.",
                        defaultReasoningEffort = "high",
                        supportedReasoningEfforts = efforts,
                    ),
                    CodingAgentModel(
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
            CairnTheme(theme) {
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

    @Test
    fun oneModelOfferedByTwoConnectionsRunsAutoOrOnAPinnedOne() {
        val shared = listOf("medium", "high").map { ReasoningOption(it) }
        val catalog =
            ModelCatalog(
                listOf(
                    CodingAgentModel(
                        "auto/claude-sonnet-4-5",
                        routes =
                            listOf(
                                ModelRoute(
                                    "opencode-go/claude-sonnet-4.5",
                                    "opencode-go",
                                    "OpenCode Go",
                                    "subscription",
                                    supportedReasoningEfforts = shared,
                                ),
                                ModelRoute(
                                    "opencode/claude-sonnet-4-5",
                                    "opencode",
                                    "OpenCode Zen",
                                    "credits",
                                    supportedReasoningEfforts = shared + ReasoningOption("max"),
                                ),
                            ),
                        displayName = "Claude Sonnet 4.5",
                        description = "Via OpenCode Go, then OpenCode Zen",
                        isDefault = true,
                        supportedReasoningEfforts = shared,
                    )
                )
            )
        var chosen = "auto/claude-sonnet-4-5" to "high"
        compose.setContent {
            var model by remember { mutableStateOf(chosen.first) }
            var reasoning by remember { mutableStateOf(chosen.second) }
            CairnTheme("light") {
                Surface(Modifier.fillMaxSize()) {
                    ModelPicker(catalog, model, reasoning, provider = "opencode") { m, r ->
                        model = m
                        reasoning = r
                        chosen = m to r
                    }
                }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        compose
            .onNodeWithText("Essaie OpenCode Go, puis OpenCode Zen", substring = true)
            .assertExists()
        capture("opencode-routes-auto")
        compose.onNodeWithText("OpenCode Zen · Crédits").performClick()
        compose.waitForIdle()
        // Pinning keeps the model and a reasoning level both routes offer.
        assert(chosen == ("opencode/claude-sonnet-4-5" to "high")) { chosen.toString() }
        compose.onNodeWithText("Uniquement sur OpenCode Zen", substring = true).assertExists()
        capture("opencode-routes-pinned")
        compose.onNodeWithText("Auto").performClick()
        compose.waitForIdle()
        assert(chosen.first == "auto/claude-sonnet-4-5") { chosen.toString() }
    }
}
