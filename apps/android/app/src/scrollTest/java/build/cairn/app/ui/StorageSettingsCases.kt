package build.cairn.app.ui

import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import build.cairn.contracts.SaveStorageSettingsInput
import build.cairn.contracts.StorageSettings
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

abstract class StorageSettingsCases {
    @get:Rule val compose = createComposeRule()

    private val configured =
        StorageSettings(
            bucket = "cairn-disks",
            configured = true,
            endpoint = "https://s3.example.test",
            environmentManaged = false,
            integrated = false,
            region = "eu-west-3",
        )

    @Test
    fun ownerChecksAndReplacesStorageWithoutSeeingCredentialsAgain() {
        val saves = CopyOnWriteArrayList<SaveStorageSettingsInput>()
        var checks = 0
        compose.setContent {
            MaterialTheme {
                Page {
                    StorageSettings(
                        load = { configured },
                        save = {
                            saves += it
                            configured.copy(bucket = it.bucket, endpoint = it.endpoint)
                        },
                        check = { checks++ },
                    )
                }
            }
        }
        compose.onNodeWithText("Bucket S3 : cairn-disks · https://s3.example.test").assertExists()
        compose.onNodeWithText("Vérifier le stockage").performScrollTo().performClick()
        compose.waitUntil { checks == 1 }
        compose.onNodeWithText("Écriture, lecture et suppression réussies").assertExists()

        compose.onNodeWithText("Configurer un S3 externe").performClick()
        compose.onNodeWithText("Endpoint S3").performTextInput("https://s3.other.test")
        compose.onNodeWithText("Bucket S3").performTextInput("other-disks")
        compose.onNodeWithText("Région S3").performTextInput("fr-par")
        compose.onNodeWithText("Clé d’accès S3").performTextInput("fixture-access")
        compose
            .onNodeWithText("Clé secrète S3")
            .performScrollTo()
            .performTextInput("fixture-secret")
        compose.onNodeWithText("Enregistrer le stockage S3").performScrollTo().performClick()
        compose.waitUntil { saves.size == 1 }
        assertEquals("fixture-secret", saves.single().secretAccessKey)
        assertEquals(false, saves.single().privateBucketConfirmed)
        compose.onNodeWithText("Stockage enregistré").assertExists()
        compose.onNodeWithText("Bucket S3 : other-disks · https://s3.other.test").assertExists()
        compose.onAllNodesWithText("fixture-secret", substring = true).assertCountEquals(0)
    }

    @Test
    fun r2RequiresThePrivacyConfirmationAndEnvironmentConfigurationLocksTheForm() {
        compose.setContent {
            MaterialTheme {
                Page {
                    StorageSettings(load = { configured }, save = { configured }, check = {})
                }
            }
        }
        compose.onNodeWithText("Configurer un S3 externe").performClick()
        compose
            .onNodeWithText("Endpoint S3")
            .performTextInput("https://11111111111111111111111111111111.r2.cloudflarestorage.com")
        compose.onNodeWithText("Bucket S3").performTextInput("cairn-disks")
        compose.onNodeWithText("Région S3").performTextInput("auto")
        compose.onNodeWithText("Clé d’accès S3").performTextInput("fixture-access")
        compose
            .onNodeWithText("Clé secrète S3")
            .performScrollTo()
            .performTextInput("fixture-secret")
        compose.onNodeWithText("Enregistrer le stockage S3").performScrollTo().assertIsNotEnabled()
        compose
            .onNodeWithText("Je confirme que les domaines publics R2", substring = true)
            .performScrollTo()
            .performClick()
        compose.onNodeWithText("Enregistrer le stockage S3").assertIsEnabled()
    }

    @Test
    fun environmentManagedStorageCannotBeEditedHere() {
        compose.setContent {
            MaterialTheme {
                Page {
                    StorageSettings(
                        load = { configured.copy(environmentManaged = true) },
                        save = { configured },
                        check = {},
                    )
                }
            }
        }
        compose
            .onNodeWithText("configuré par des variables d’environnement", substring = true)
            .assertExists()
        compose.onNodeWithText("Configurer un S3 externe").assertIsNotEnabled()
    }
}
