package build.cairn.app.ui

import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Before
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36], qualifiers = "w412dp-h915dp-mdpi")
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class ClientUpdateTest : ClientUpdateCases() {
    @Before
    fun initializeWork() {
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
}
