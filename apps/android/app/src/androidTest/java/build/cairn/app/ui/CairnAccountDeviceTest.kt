package build.cairn.app.ui

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.KeystoreSessionVault
import build.cairn.app.data.SessionVault
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CairnAccountDeviceTest : CairnAccountCases() {
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
        unavailableBeaconServiceRetriesTheStoredSessionAndOnlyRejectionSignsOut()
}
