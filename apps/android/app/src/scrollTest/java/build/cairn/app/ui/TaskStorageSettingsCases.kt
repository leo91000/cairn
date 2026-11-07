package build.cairn.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.contracts.TaskStorageLimits
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

abstract class TaskStorageSettingsCases {
    @get:Rule val compose = createComposeRule()
    private val field = "Disque maximal par tâche (Gio)"
    private val button = "Enregistrer la limite"

    @Test
    fun customInstallationLimitLoadsAndSavesWithItsRevision() {
        val saves = CopyOnWriteArrayList<TaskStorageLimits>()
        compose.setContent {
            MaterialTheme {
                Page {
                    TaskStorageSettings(
                        load = { TaskStorageLimits(75, "revision-1") },
                        save = {
                            saves += it
                            it.copy(revision = "revision-2")
                        },
                    )
                }
            }
        }
        compose.onNodeWithText(field).performScrollTo().assertTextContains("75")
        compose.onNodeWithText(button).assertIsNotEnabled()
        compose.onNodeWithText(field).performTextReplacement("150")
        compose.onNodeWithText(button).performScrollTo().performClick()
        compose.waitUntil { saves.size == 1 }
        assertEquals(TaskStorageLimits(150, "revision-1"), saves.single())
        compose.onNodeWithText("Limite enregistrée").assertExists()
        compose.onNodeWithText(button).assertIsNotEnabled()
    }

    @Test
    fun invalidValuesStayEditableAndCannotBeSaved() {
        compose.setContent {
            MaterialTheme {
                Page {
                    TaskStorageSettings(load = { TaskStorageLimits(150, "current") }, save = { it })
                }
            }
        }
        for (invalid in listOf("31", "1025", "1.5", "")) {
            compose.onNodeWithText(field).performScrollTo().performTextReplacement(invalid)
            compose.onNodeWithText(button).assertIsNotEnabled()
        }
        compose.onNodeWithText(field).performTextReplacement("200")
        compose.onNodeWithText(button).assertIsEnabled()
    }

    @Test
    fun conflictKeepsTheDraftUntilExplicitReload() {
        var loads = 0
        compose.setContent {
            MaterialTheme {
                Page {
                    TaskStorageSettings(
                        load = {
                            loads++
                            TaskStorageLimits(if (loads == 1) 150 else 100, "revision-$loads")
                        },
                        save = { error("Limite modifiée ailleurs") },
                    )
                }
            }
        }
        compose.onNodeWithText(field).performScrollTo().performTextReplacement("200")
        compose.onNodeWithText(button).performScrollTo().performClick()
        compose.onNodeWithText("Limite modifiée ailleurs").assertExists()
        compose.onNodeWithText(field).performScrollTo().assertTextContains("200")
        compose.onNodeWithText("Recharger la limite enregistrée").performScrollTo().performClick()
        compose.onNodeWithText(field).performScrollTo().assertTextContains("100")
        compose.onNodeWithText(button).assertIsNotEnabled()
    }

    @Test
    fun failedInitialLoadCanBeRetried() {
        var loads = 0
        compose.setContent {
            MaterialTheme {
                Page {
                    TaskStorageSettings(
                        load = {
                            loads++
                            if (loads == 1) error("Chargement impossible")
                            TaskStorageLimits(150, "current")
                        },
                        save = { it },
                    )
                }
            }
        }
        compose.onNodeWithText("Chargement impossible").assertExists()
        compose.onNodeWithText("Recharger la limite enregistrée").performScrollTo().performClick()
        compose.onNodeWithText(field).performScrollTo().assertTextContains("150")
    }
}
