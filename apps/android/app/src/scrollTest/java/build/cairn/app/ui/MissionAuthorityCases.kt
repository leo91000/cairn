package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import org.junit.Rule
import org.junit.Test

abstract class MissionAuthorityCases {
    @get:Rule val compose = createComposeRule()

    @Test
    fun anotherAuthorsCommitmentCanBeArchivedButNotEditedResumedRestoredOrDeleted() {
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                beaconOrigin = "",
            )
        val models = ViewModelStore().apply { put("mission-authority", vm) }
        val task =
            mutableStateOf(
                Task(
                        name = "Engagement partagé",
                        prompt = "Travail approuvé",
                        cron = "0 9 * * *",
                        enabled = false,
                    )
                    .copy(authorId = "another-account")
            )
        val state =
            Workspace(
                installation = Installation("fixture", "Partagée", InstallationRole.Member),
                session = Session(account = CairnAccount("member", "member@example.test")),
            )
        val mounted = mutableStateOf(true)
        try {
            compose.setContent {
                if (mounted.value)
                    CairnTheme {
                        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                            MissionPanel(vm, state, task.value, {}, {})
                        }
                    }
            }
            compose
                .onNodeWithContentDescription("Modifier la mission")
                .performScrollTo()
                .assertIsNotEnabled()
            compose.onNodeWithContentDescription("Reprendre la planification").assertIsNotEnabled()
            compose.onNodeWithContentDescription("Autres actions").performScrollTo().performClick()
            compose.onNodeWithText("Archiver").assertIsEnabled()
            compose.onNodeWithText("Supprimer").assertDoesNotExist()
            compose.onNodeWithText("Dupliquer (en pause)").assertIsEnabled()
            compose.runOnIdle { task.value = task.value.copy(archived = true) }
            compose.onNodeWithText("Restaurer (en pause)").assertIsNotEnabled()
        } finally {
            compose.runOnIdle { mounted.value = false }
            models.clear()
        }
    }
}
