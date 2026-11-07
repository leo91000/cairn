package build.cairn.app.data

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import kotlinx.coroutines.test.runTest
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class FilesTest {
    @Test
    fun `reusing attachments keeps the message within count and byte limits`() {
        val full =
            (1..4).map {
                DraftAttachment(
                    ChatAttachment(
                        id = "file-$it",
                        chatId = "chat",
                        name = "file-$it.zip",
                        size = MAX_ATTACHMENT_BYTES.toInt(),
                    )
                )
            }
        assertSame(full, reuseAttachment(full, full.first().attachment))
        val extra = ChatAttachment(id = "extra", chatId = "chat", name = "extra.zip", size = 1)
        val sizeFailure =
            assertThrows(IllegalArgumentException::class.java) {
                reuseAttachment(full, extra)
            }
        assertTrue(sizeFailure.message!!.contains("40 Mo"))
        val partial = full.dropLast(1)
        val reused = reuseAttachment(partial, full.last().attachment)
        assertEquals(full, reused)
        assertNull(reused.last().localPath)
        val many = (1..8).map { DraftAttachment(extra.copy(id = "small-$it")) }
        assertTrue(
            assertThrows(IllegalArgumentException::class.java) {
                    reuseAttachment(many, extra)
                }
                .message!!
                .contains("8 fichiers")
        )
    }

    @Test
    fun `identical artifact paths remain isolated across installations and account sessions`() =
        runTest {
            MockWebServer().use { server ->
                server.enqueue(MockResponse().setBody("Maison"))
                server.enqueue(MockResponse().setBody("Bureau"))
                server.enqueue(MockResponse().setBody("Autre compte"))
                server.start()

                val application = ApplicationProvider.getApplicationContext<Application>()
                val files = Files(application)
                val home = CairnApi(server.url("/"), MemoryVault(), installationId = "home")
                val work = CairnApi(server.url("/"), MemoryVault(), installationId = "work")
                val otherAccount = CairnApi(server.url("/"), MemoryVault(), installationId = "work")
                home.csrf = "first-account"
                work.csrf = "first-account"
                otherAccount.csrf = "second-account"
                val path = "/runs/restored-run/artifacts/same-file"

                assertEquals("Maison", files.fetch(home, path, "note.txt").readText())
                assertEquals("Bureau", files.fetch(work, path, "note.txt").readText())
                assertEquals("Autre compte", files.fetch(otherAccount, path, "note.txt").readText())
                assertEquals("Bureau", files.fetch(work, path, "note.txt").readText())
                assertEquals(3, server.requestCount)
                assertEquals(
                    "/api/installations/home/api/runs/restored-run/artifacts/same-file",
                    server.takeRequest().path,
                )
                repeat(2) {
                    assertEquals(
                        "/api/installations/work/api/runs/restored-run/artifacts/same-file",
                        server.takeRequest().path,
                    )
                }
            }
        }
}
