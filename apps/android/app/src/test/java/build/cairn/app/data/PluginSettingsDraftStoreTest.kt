package build.cairn.app.data

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.lang.management.ManagementFactory
import java.util.UUID
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class PluginSettingsDraftStoreTest {
    private val application = ApplicationProvider.getApplicationContext<Application>()
    private val root
        get() = File(application.noBackupFilesDir, "plugin-setup-drafts")

    @Before fun initialize() = PluginSettingsDraftStore.clear(application)

    @After fun cleanup() = PluginSettingsDraftStore.clear(application)

    @Test
    fun ordinaryInputsRoundTripWithoutCredentialsOrCrossingScopes() {
        val store =
            PluginSettingsDraftStore(
                application,
                "account/installation/plugin/draft",
                setOf("label"),
            )
        val key = UUID.randomUUID().toString()
        assertTrue(store.write(key, mapOf("label" to "unsaved edit", "secret" to "fixture-secret")))
        assertEquals(mapOf("label" to "unsaved edit"), store.read(key))
        assertFalse(root.listFiles()!!.single().readText().contains("fixture-secret"))
        assertNull(
            PluginSettingsDraftStore(application, "another-account", setOf("label")).read(key)
        )
        assertNull(store.read("../outside"))
        assertFalse(store.write("../outside", mapOf("label" to "value")))
        store.remove(key)
        assertNull(store.read(key))
    }

    @Test
    fun recoveryHonoursAtomicBackupsExpiryAndCorruptSnapshots() {
        val store = PluginSettingsDraftStore(application, "scope", setOf("label"))
        val key = UUID.randomUUID().toString()
        val values = mapOf("label" to "retained")
        assertTrue(store.write(key, values))
        val file = root.listFiles()!!.single()
        assertTrue(file.renameTo(File(file.path + ".bak")))
        assertEquals(values, store.read(key))
        file.writeText("{")
        assertNull(store.read(key))
        assertTrue(store.write(key, values))
        assertTrue(file.setLastModified(System.currentTimeMillis() - 25 * 60 * 60 * 1000L))
        assertNull(store.read(key))
    }

    @Test
    fun clearingPrivateStateRetiresExistingHandles() {
        val store = PluginSettingsDraftStore(application, "scope", setOf("label"))
        val key = UUID.randomUUID().toString()
        assertTrue(store.write(key, mapOf("label" to "draft")))
        PluginSettingsDraftStore.clear(application)
        assertNull(store.read(key))
        assertFalse(store.write(key, mapOf("label" to "late save from a retired editor")))
        assertFalse(root.exists())
        val next = PluginSettingsDraftStore(application, "new-scope", setOf("label"))
        assertTrue(next.write(key, mapOf("label" to "new draft")))
        assertEquals(mapOf("label" to "new draft"), next.read(key))
    }

    @Test
    fun largeOrdinaryInputsUseBoundedLocalStorage() {
        val workloads =
            mapOf(
                "setup" to
                    (0 until 20).associate {
                        "field$it" to if (it < 6) "\"".repeat(10000) else "x".repeat(10000)
                    },
                "unsaved-resource" to
                    (0 until 29).associate {
                        "field$it" to "\"".repeat(10000)
                    },
                "grant-scopes" to
                    (0 until 8).associate { n ->
                        "scope$n" to
                            (0 until 100).joinToString("\n") { i ->
                                "$n-${i.toString().padStart(3, '0')}-${"x".repeat(172)}"
                            }
                    },
            )
        workloads.forEach { (name, values) ->
            PluginSettingsDraftStore.clear(application)
            val key = UUID.randomUUID().toString()
            val store = PluginSettingsDraftStore(application, name, values.keys)
            val cpu = ManagementFactory.getThreadMXBean()
            val cpuBefore = cpu.currentThreadCpuTime
            val writeStart = System.nanoTime()
            assertTrue(store.write(key, values))
            val writeNanos = System.nanoTime() - writeStart
            val firstReadStart = System.nanoTime()
            assertEquals(values, PluginSettingsDraftStore(application, name, values.keys).read(key))
            val firstReadNanos = System.nanoTime() - firstReadStart
            val warmReadStart = System.nanoTime()
            repeat(5) { assertEquals(values, store.read(key)) }
            val warmReadNanos = (System.nanoTime() - warmReadStart) / 5
            val cpuNanos = cpu.currentThreadCpuTime - cpuBefore
            val encodedBytes = root.listFiles()!!.single().length()
            println(
                "Plugin ordinary draft storage: workload=$name encodedBytes=$encodedBytes writeNanos=$writeNanos firstNewStoreReadNanos=$firstReadNanos repeatedReadMeanNanos=$warmReadNanos threadCpuNanos=$cpuNanos; first/new-store read uses OS page cache, not a physical cold read"
            )
            assertTrue(encodedBytes < if (name == "setup") 300000 else 600000)
            assertEquals(values, store.read(key))
        }
        PluginSettingsDraftStore.clear(application)
        val store = PluginSettingsDraftStore(application, "retention", setOf("field0"))
        repeat(34) {
            assertTrue(store.write(UUID.randomUUID().toString(), mapOf("field0" to "draft $it")))
        }
        assertTrue(root.listFiles()!!.size <= 32)
    }
}
