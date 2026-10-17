package build.cairn.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asAndroidBitmap
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import java.io.File
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-xhdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class PlanViewsTest {
    @get:Rule val compose = createComposeRule()

    private val working =
        AgentPlan(
            listOf(
                PlanStep("Explorer le middleware d’authentification", "", PlanStepState.DONE),
                PlanStep("Ajouter la migration d’expiration", "", PlanStepState.DONE),
                PlanStep(
                    "Rafraîchir les jetons dans le middleware",
                    "Rafraîchissement des jetons",
                    PlanStepState.ACTIVE,
                ),
                PlanStep("Couvrir l’expiration par des tests", "", PlanStepState.PENDING),
                PlanStep("Rédiger le changelog", "", PlanStepState.CANCELLED),
            ),
            "Le modèle de données d’abord, puis le middleware.",
            1,
            2,
        )

    @Composable
    private fun Fixture(theme: String = "light", content: @Composable ColumnScope.() -> Unit) {
        CairnTheme(theme) {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                Column(
                    Modifier.padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                    content = content,
                )
            }
        }
    }

    @Test
    fun `the card follows the plan and folds once it is finished`() {
        var plan by mutableStateOf(working)
        compose.setContent {
            Fixture {
                PlanCard(plan, live = true)
                PlanUpdateRow(
                    PlanUpdate(
                        "plan:1",
                        PlanChange(
                            done = listOf("Ajouter la migration d’expiration"),
                            started = listOf("Rafraîchir les jetons dans le middleware"),
                            cancelled = listOf("Rédiger le changelog"),
                        ),
                        PlanProgress(2, 4),
                    )
                )
            }
        }
        compose.onNodeWithText("EN DIRECT").assertIsDisplayed()
        compose.onNodeWithText("5 étapes", substring = true).assertDoesNotExist()
        compose.onNodeWithText("4 étapes · 2 terminée(s)").assertIsDisplayed()
        compose
            .onNodeWithText("Le modèle de données d’abord, puis le middleware.")
            .assertIsDisplayed()
        compose.onNodeWithText("Rafraîchissement des jetons").assertIsDisplayed()
        compose.onAllNodesWithTag("plan-step").assertCountEquals(5)
        compose
            .onNodeWithTag("plan-update")
            .assertContentDescriptionContains(
                "terminé Ajouter la migration d’expiration",
                substring = true,
            )
            .assertContentDescriptionContains("2 sur 4", substring = true)
        shot("agent-plan-card")

        compose.runOnIdle {
            plan =
                plan.copy(
                    steps =
                        plan.steps.map {
                            it.copy(
                                state =
                                    if (it.state == PlanStepState.CANCELLED) it.state
                                    else PlanStepState.DONE
                            )
                        }
                )
        }
        compose.onNodeWithText("Plan terminé").assertIsDisplayed()
        compose.onAllNodesWithTag("plan-step").assertCountEquals(0)
        compose.onNodeWithText("Plan terminé").performClick()
        compose.onAllNodesWithTag("plan-step").assertCountEquals(5)
    }

    @Test
    fun `the bar names the step in progress and opens the whole plan`() {
        compose.setContent {
            Fixture("dark") {
                Spacer(Modifier.weight(1f))
                PlanBar(working)
            }
        }
        compose
            .onNodeWithTag("plan-bar")
            .assertContentDescriptionEquals(
                "Plan, 2 sur 4 terminée(s). Rafraîchissement des jetons. Afficher le plan"
            )
        shot("agent-plan-bar")
        compose.onNodeWithTag("plan-bar").performClick()
        compose.onNodeWithTag("plan-sheet").assertIsDisplayed()
        compose.onNodeWithText("2 sur 4").assertIsDisplayed()
        compose
            .onNodeWithText("Le modèle de données d’abord, puis le middleware.")
            .assertIsDisplayed()
        shot("agent-plan-sheet")
    }

    @Test
    fun `without a step in progress the bar names the next one`() {
        val waiting =
            working.copy(
                steps =
                    working.steps.map {
                        if (it.state == PlanStepState.ACTIVE) it.copy(state = PlanStepState.PENDING)
                        else it
                    }
            )
        compose.setContent { Fixture { PlanBar(waiting) } }
        compose
            .onNodeWithTag("plan-bar")
            .assertContentDescriptionContains(
                "Suivant : Rafraîchir les jetons dans le middleware",
                substring = true,
            )
    }

    private fun shot(name: String) {
        val dir = System.getProperty("cairn.screenshots.dir") ?: return
        File(dir).mkdirs()
        File(dir, "$name.png").outputStream().use {
            compose
                .onRoot()
                .captureToImage()
                .asAndroidBitmap()
                .compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
    }
}
