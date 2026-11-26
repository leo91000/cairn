package build.cairn.app

import android.app.Application
import android.content.Intent
import android.os.Bundle
import androidx.compose.runtime.MutableState
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class MainActivityShareTest {
    @Before
    fun initializeWork() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            ApplicationProvider.getApplicationContext<Application>(),
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
    }

    @After
    fun closeWork() {
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun `process restore keeps the waiting share instead of replaying the launch share`() {
        fun share(url: String) =
            Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, url)

        // The system restores the original launch intent after process death.
        val launch = share("https://cairn.example/authorize?client_id=first")
        val first = Robolectric.buildActivity(MainActivity::class.java, launch).setup()
        assertEquals("https://cairn.example/authorize?client_id=first", first.get().pendingShare())
        first.newIntent(share("https://cairn.example/authorize?client_id=second"))
        val saved = Bundle()
        first.saveInstanceState(saved)
        first.pause().stop().destroy()

        val restored =
            Robolectric.buildActivity(
                    MainActivity::class.java,
                    share("https://cairn.example/authorize?client_id=first"),
                )
                .setup(saved)
        assertEquals(
            "https://cairn.example/authorize?client_id=second",
            restored.get().pendingShare(),
        )
        restored.pause().stop().destroy()
    }

    @Suppress("UNCHECKED_CAST")
    private fun MainActivity.pendingShare(): String =
        MainActivity::class
            .java
            .getDeclaredField("sharedUrl\$delegate")
            .apply { isAccessible = true }
            .get(this)
            .let { (it as MutableState<String>).value }
}
