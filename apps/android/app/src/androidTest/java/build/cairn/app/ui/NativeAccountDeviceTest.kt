package build.cairn.app.ui

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import build.cairn.app.data.KeystoreSessionVault
import build.cairn.app.data.SessionVault
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativeAccountDeviceTest : NativeAccountCases() {
    @Before
    fun notificationsPermission() {
        val instrumentation =
            androidx.test.platform.app.InstrumentationRegistry.getInstrumentation()
        android.os.ParcelFileDescriptor.AutoCloseInputStream(
                instrumentation.uiAutomation.executeShellCommand(
                    "pm grant ${instrumentation.targetContext.packageName} android.permission.POST_NOTIFICATIONS"
                )
            )
            .use { it.readBytes() }
    }

    override fun sessionVault(application: Application): SessionVault =
        KeystoreSessionVault(application)

    @Test fun refusedPushRenewal() = refusedPushRenewalTurnsOffAndExplainsRecovery()

    @Test fun notificationTarget() = notificationOpensOnlyItsAccessibleInstallation()

    @Test fun googleSignIn() = providerOpensTheCairnAccount("Google")

    @Test fun githubSignIn() = providerOpensTheCairnAccount("GitHub")

    @Test fun passkeySignIn() = providerOpensTheCairnAccount("une passkey")
}
