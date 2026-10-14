package build.cairn.app.diagnostics

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.Preferences
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class TelemetryPreferenceTest {
    @Test
    fun `sharing defaults to enabled and an opt out persists when preferences reopen`() =
        runBlocking {
            val context = ApplicationProvider.getApplicationContext<Application>()
            val preferences = Preferences(context)
            assertTrue(preferences.telemetryEnabled.first())
            try {
                preferences.setTelemetryEnabled(false)
                assertFalse(Preferences(context).telemetryEnabled.first())
            } finally {
                preferences.setTelemetryEnabled(true)
            }
        }
}
