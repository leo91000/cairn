package dev.leo.manager.ui

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.leo.manager.data.KeystoreSessionVault
import dev.leo.manager.data.SessionVault
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LeoAccountDeviceTest : LeoAccountCases() {
    override fun sessionVault(application: Application): SessionVault =
        KeystoreSessionVault(application)

    @Test
    fun emailCodeOpensInstallationAndLogsOut() =
        emailCodeOpensTheOnlyInstallationAndLogoutRevokesTheAccount()

    @Test
    fun installationSelectionAndMemberRole() =
        switchingToASharedInstallationHidesManagementAndRestoresTheSelection()

    @Test
    fun storedSessionSurvivesNetworkFailure() =
        unavailableOfficialServiceRetriesTheStoredSessionAndOnlyRejectionSignsOut()
}
