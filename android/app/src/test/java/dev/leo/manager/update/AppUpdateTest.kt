package dev.leo.manager.update

import java.io.ByteArrayOutputStream
import org.junit.Assert.*
import org.junit.Test

class AppUpdateTest {
    private val update =
        AppUpdate(
            1,
            "0.29.0",
            100029000,
            26,
            "$RELEASE_BASE/download/v0.29.0/leo-android.apk",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            3,
        )

    @Test
    fun acceptsOnlyCompatibleNewerVersions() {
        update.validate()
        assertTrue(update.isNewer(28, 26))
        assertFalse(update.isNewer(100029000, 37))
        assertFalse(update.isNewer(100030000, 37))
        assertFalse(update.isNewer(28, 25))
    }

    @Test
    fun rejectsUntrustedSourcesAndInvalidMetadata() {
        listOf(
                update.copy(url = "https://evil.example/app.apk"),
                update.copy(url = update.url.replace("https:", "http:")),
                update.copy(versionName = "../bad"),
                update.copy(size = MAX_APK_BYTES + 1),
                update.copy(sha256 = "invalid"),
                update.copy(schema = 2),
            )
            .forEach {
                assertThrows(IllegalArgumentException::class.java) { it.validate() }
            }
    }

    @Test
    fun verifiesStreamingDownloadAndRejectsCorruptionOrTruncation() {
        val output = ByteArrayOutputStream()
        copyVerified("abc".byteInputStream(), output, update) {}
        assertEquals("abc", output.toString())
        listOf("ab", "abcd", "xyz").forEach {
            assertThrows(IllegalArgumentException::class.java) {
                copyVerified(it.byteInputStream(), ByteArrayOutputStream(), update) {}
            }
        }
    }
}
