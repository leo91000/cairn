package dev.leo.manager.ui

import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class NativeAccountTest : NativeAccountCases() {
    @Before
    fun initializeWork() {
        org.robolectric.Shadows.shadowOf(
                ApplicationProvider.getApplicationContext<android.app.Application>()
            )
            .grantPermissions(android.Manifest.permission.POST_NOTIFICATIONS)
        androidx.work.testing.WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext(),
            androidx.work.Configuration.Builder()
                .setExecutor(androidx.work.testing.SynchronousExecutor())
                .build(),
        )
    }

    @After
    fun closeWork() {
        androidx.work.testing.WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test fun refusedPushRenewal() = refusedPushRenewalTurnsOffAndExplainsRecovery()

    @Test fun notificationTarget() = notificationOpensOnlyItsAccessibleInstallation()

    @Test fun googleSignIn() = providerOpensTheLeoAccount("Google")

    @Test fun githubSignIn() = providerOpensTheLeoAccount("GitHub")

    @Test fun passkeySignIn() = providerOpensTheLeoAccount("une passkey")
}
