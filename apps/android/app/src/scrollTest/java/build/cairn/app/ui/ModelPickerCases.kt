package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.Density
import build.cairn.app.data.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

abstract class ModelPickerCases {
    @get:Rule val compose = createComposeRule()
    private val catalog =
        ModelCatalog(
            models =
                listOf(
                    CodingAgentModel(
                        "large",
                        "Grand modèle",
                        isDefault = true,
                        defaultReasoningEffort = "medium",
                        supportedReasoningEfforts =
                            listOf("low", "medium", "high", "max", "ultra").map {
                                ReasoningOption(it)
                            },
                    ),
                    CodingAgentModel(
                        "fast",
                        "Modèle rapide",
                        defaultReasoningEffort = "low",
                        supportedReasoningEfforts =
                            listOf(ReasoningOption("low"), ReasoningOption("high")),
                    ),
                    CodingAgentModel("hidden", "Modèle masqué", hidden = true),
                ) + (1..5).map { CodingAgentModel("extra-$it", "Modèle annexe $it") }
        )

    protected open fun captureReasoning() = Unit

    protected open fun selectMaximumEffort() {
        compose.onNodeWithTag("reasoning-slider").performTouchInput {
            swipe(center, androidx.compose.ui.geometry.Offset(width - 24f, centerY))
        }
    }

    @Test
    fun reasoningSliderRespondsToTouch() {
        var result = "high"
        compose.setContent {
            var effort by remember { mutableStateOf("high") }
            CairnTheme("dark") {
                SurfaceForTest {
                    ReasoningControl(catalog.models.first(), effort, "high", true) {
                        effort = it
                        result = it
                    }
                }
            }
        }
        compose.onNodeWithTag("reasoning-slider").performTouchInput {
            swipe(center, androidx.compose.ui.geometry.Offset(width - 24f, centerY))
        }
        compose.runOnIdle { assertEquals("ultra", result) }
    }

    @Test
    fun selectionSurvivesRecreationAndModelChangeResetsEffort() {
        val restoration = StateRestorationTester(compose)
        var result = "" to ""
        restoration.setContent {
            var model by rememberSaveable { mutableStateOf("") }
            var reasoning by rememberSaveable { mutableStateOf("") }
            CairnTheme("dark") {
                SurfaceForTest {
                    ModelPicker(catalog, model, reasoning, "large", "high", inherit = true) { m, r
                        ->
                        model = m
                        reasoning = r
                        result = m to r
                    }
                }
            }
        }
        compose
            .onNodeWithTag("model-picker")
            .assertTextContains("Grand modèle")
            .assertTextContains("Élevé")
            .performClick()
        selectMaximumEffort()
        compose.runOnIdle { assertEquals("ultra", result.second) }
        captureReasoning()
        compose.onNodeWithText("Terminé").performClick()
        restoration.emulateSavedInstanceStateRestore()
        compose.onNodeWithTag("model-picker").assertTextContains("Ultra").performClick()
        compose.onNodeWithTag("reasoning-default").performScrollTo().performClick()
        compose
            .onNodeWithTag("reasoning-slider")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "Élevé"))
        compose.onNodeWithTag("reasoning-slider").performSemanticsAction(
            SemanticsActions.SetProgress
        ) {
            it(3f)
        }
        compose.runOnIdle { assertEquals("max", result.second) }
        compose.onNodeWithText("Modèle masqué").assertDoesNotExist()
        compose.onNodeWithText("Rechercher un modèle").performTextInput("rapide")
        compose.onNodeWithText("Modèle annexe 1").assertDoesNotExist()
        compose.onNodeWithText("Modèle rapide").performClick()
        compose.runOnIdle { assertEquals("fast" to "", result) }
        // The effort control follows the chosen model inside the same sheet.
        compose
            .onNodeWithTag("reasoning-slider")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "Faible"))
            .performSemanticsAction(SemanticsActions.SetProgress) { it(1f) }
        compose.runOnIdle { assertEquals("fast" to "high", result) }
        compose.onNodeWithText("Terminé").performClick()
        compose
            .onNodeWithTag("model-picker")
            .assertTextContains("Modèle rapide")
            .assertTextContains("Élevé")
    }

    @Test
    fun claudeDefaultAliasIsMergedIntoTheDefaultRow() {
        val claude =
            ModelCatalog(
                listOf(
                    CodingAgentModel(
                        "default",
                        "Default (recommended)",
                        "Opus avec 1M de contexte",
                        isDefault = true,
                    ),
                    CodingAgentModel("sonnet", "Sonnet"),
                )
            )
        compose.setContent {
            CairnTheme("light") {
                SurfaceForTest { ModelPicker(claude, "", "", provider = "claude") { _, _ -> } }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        compose.onAllNodes(hasText("Default (recommended)") and isSelectable()).assertCountEquals(0)
        compose
            .onNode(hasText("Modèle par défaut") and isSelectable())
            .assertIsSelected()
            .assert(hasText("Opus avec 1M de contexte"))
        compose.onNodeWithText("Sonnet").assertExists()
    }

    protected open fun captureDefaultModel() = Unit

    @Test
    fun claudeDefaultNamesRemainUnpinnedWhenMetadataChanges() {
        var name by mutableStateOf("Opus Fixture (1M context)")
        var description by mutableStateOf("Model recommended for your subscription")
        var model by mutableStateOf("sonnet")
        var reasoning by mutableStateOf("high")
        compose.setContent {
            CairnTheme("dark") {
                SurfaceForTest {
                    ModelPicker(
                        ModelCatalog(
                            listOf(
                                CodingAgentModel("default", name, description, isDefault = true),
                                CodingAgentModel("sonnet", "Sonnet"),
                            )
                        ),
                        model,
                        reasoning,
                        provider = "claude",
                    ) { m, r ->
                        model = m
                        reasoning = r
                    }
                }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        val scenarios =
            listOf(
                Triple(
                    "Opus Fixture (1M context)",
                    "Model recommended for your subscription",
                    "Modèle par défaut · Opus Fixture (1M context)",
                ),
                Triple(
                    "Opus description",
                    "Opus description · Details",
                    "Modèle par défaut · Opus description",
                ),
                Triple(
                    "claude-opus-fixture[1m]",
                    "",
                    "Modèle par défaut · claude-opus-fixture[1m]",
                ),
                Triple("Default (recommended)", "", "Modèle par défaut"),
            )
        for ((display, details, title) in scenarios) {
            compose.runOnIdle {
                name = display
                description = details
            }
            val default = compose.onNode(hasText(title) and isSelectable())
            default.performClick().assertIsSelected()
            compose.runOnIdle {
                assertEquals("", model)
                assertEquals("", reasoning)
            }
            if (display == "Opus Fixture (1M context)") captureDefaultModel()
            compose.onNodeWithText("Terminé").performClick()
            compose.onNodeWithTag("model-picker").assertTextContains(display).performClick()
            compose.onNode(hasText("Sonnet") and isSelectable()).performClick()
            compose.runOnIdle { assertEquals("sonnet", model) }
        }
    }

    @Test
    fun unknownModelDoesNotBorrowDefaultModelsEfforts() {
        compose.setContent {
            CairnTheme("light") {
                SurfaceForTest { ModelPicker(catalog, "missing", "max") { _, _ -> } }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        compose.onNodeWithTag("reasoning-slider").assertDoesNotExist()
        compose
            .onNodeWithText(
                "Catalogue indisponible pour ce modèle. Le réglage enregistré est conservé."
            )
            .assertExists()
    }

    @Test
    fun enlargedTextAndUnsupportedSavedEffortRemainRecoverable() {
        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, 1.6f)) {
                var effort by remember { mutableStateOf("ultra") }
                CairnTheme("dark") {
                    SurfaceForTest { ModelPicker(catalog, "fast", effort) { _, r -> effort = r } }
                }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        compose
            .onNodeWithText(
                "Ce niveau n’est pas proposé par le modèle. Choisissez un niveau disponible ou le réglage par défaut."
            )
            .assertExists()
        compose.onNodeWithTag("reasoning-default").performScrollTo().performClick()
        compose
            .onNodeWithTag("reasoning-slider")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, "Faible"))
    }

    @Composable
    private fun SurfaceForTest(content: @Composable () -> Unit) {
        androidx.compose.material3.Surface(Modifier.fillMaxSize()) { Column { content() } }
    }

    @Test
    fun inheritedPinnedModelDropsRouteOnlyReasoningWhenReturningToAuto() {
        val shared = listOf("medium", "high").map { ReasoningOption(it) }
        val catalog =
            ModelCatalog(
                listOf(
                    CodingAgentModel(
                        "auto/sonnet",
                        displayName = "Sonnet",
                        supportedReasoningEfforts = shared,
                        routes =
                            listOf(
                                ModelRoute(
                                    "opencode-go/sonnet",
                                    "opencode-go",
                                    "OpenCode Go",
                                    "subscription",
                                    supportedReasoningEfforts = shared,
                                ),
                                ModelRoute(
                                    "opencode/sonnet",
                                    "opencode",
                                    "OpenCode Zen",
                                    "credits",
                                    supportedReasoningEfforts = shared + ReasoningOption("max"),
                                ),
                            ),
                    )
                )
            )
        var chosen: Pair<String, String>? = null
        compose.setContent {
            var model by remember { mutableStateOf("") }
            var reasoning by remember { mutableStateOf("") }
            CairnTheme {
                SurfaceForTest {
                    ModelPicker(
                        catalog,
                        model,
                        reasoning,
                        defaultModel = "opencode/sonnet",
                        defaultReasoning = "max",
                        inherit = true,
                        conversationDefault = true,
                        provider = "opencode",
                    ) { m, r ->
                        model = m
                        reasoning = r
                        chosen = m to r
                    }
                }
            }
        }
        compose.onNodeWithTag("model-picker").performClick()
        compose.onNodeWithText("Auto", substring = false).performScrollTo().performClick()
        compose.runOnIdle { assertEquals("auto/sonnet" to "", chosen) }
    }
}
