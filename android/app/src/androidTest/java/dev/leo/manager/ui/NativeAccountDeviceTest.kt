package dev.leo.manager.ui

import android.app.Application
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.leo.manager.data.KeystoreSessionVault
import dev.leo.manager.data.SessionVault
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativeAccountDeviceTest : NativeAccountCases() {
    override fun sessionVault(application: Application): SessionVault =
        KeystoreSessionVault(application)

    @Test fun notificationTarget() = notificationOpensOnlyItsAccessibleInstallation()

    @Test fun googleSignIn() = providerOpensTheLeoAccount("Google")

    @Test fun githubSignIn() = providerOpensTheLeoAccount("GitHub")

    @Test fun passkeySignIn() = providerOpensTheLeoAccount("une passkey")
}
