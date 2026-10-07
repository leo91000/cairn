package dev.leo.manager.data

import android.Manifest
import android.content.Context
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class NativePushDeviceTest : NativePushCases() {
    override fun sessionVault(context: Context): SessionVault = KeystoreSessionVault(context)

    @Before
    fun notificationsPermission() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        android.os.ParcelFileDescriptor.AutoCloseInputStream(
                instrumentation.uiAutomation.executeShellCommand(
                    "pm grant ${instrumentation.targetContext.packageName} ${Manifest.permission.POST_NOTIFICATIONS}"
                )
            )
            .use { it.readBytes() }
    }

    @Test
    fun allInstallationsAndRevocation() =
        bothInstallationsNotifyButRemovedMembersAndLoggedOutDevicesDoNot()
}
