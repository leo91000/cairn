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
    fun revokedLastAccessExplainsTheAccountPage() =
        revokedLastInstallationShowsTheAccountPageAndExplainsTheChange()

    override fun captureRevokedAccess() {
        val instrumentation =
            androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        val screenshot = checkNotNull(instrumentation.uiAutomation.takeScreenshot())
        val file = java.io.File(instrumentation.targetContext.filesDir, "revoked-last-access.png")
        file.outputStream().use {
            screenshot.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
        }
        screenshot.recycle()
    }

    @Test
    fun storedSessionSurvivesNetworkFailure() =
        unavailableBeaconServiceRetriesTheStoredSessionAndOnlyRejectionSignsOut()
}
