package build.cairn.app.data

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import androidx.test.core.app.ApplicationProvider
import androidx.work.Data
import androidx.work.ListenableWorker
import androidx.work.testing.TestListenableWorkerBuilder
import java.io.File
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class NotificationsTest {
    @Test
    fun `persisted legacy work finishes without accessing session storage or notifications`() =
        runBlocking {
            val context =
                object : ContextWrapper(ApplicationProvider.getApplicationContext<Context>()) {
                    override fun getApplicationContext(): Context = this

                    override fun getFilesDir(): File = error("Legacy polling accessed files")

                    override fun getNoBackupFilesDir(): File =
                        error("Legacy polling accessed session storage")

                    override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
                        error("Legacy polling accessed preferences")

                    override fun getSystemService(name: String): Any =
                        error("Legacy polling accessed a system service: $name")
                }

            for (attempt in listOf(0, 3)) {
                // Use WorkManager's default reflective factory, as for a persisted worker class.
                val worker =
                    TestListenableWorkerBuilder<QuestionWorker>(context)
                        .setRunAttemptCount(attempt)
                        .setInputData(Data.Builder().putString("chat", "legacy-chat").build())
                        .build()

                assertEquals(ListenableWorker.Result.success(), worker.doWork())
            }
        }
}
