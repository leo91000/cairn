package build.cairn.app.diagnostics

import build.cairn.app.data.wireJson
import java.io.IOException
import java.util.concurrent.Executors
import kotlinx.coroutines.*
import kotlinx.serialization.encodeToString
import org.junit.Assert.*
import org.junit.Test

class PerformanceUploaderTest {
    @Test
    fun `idle windows are skipped and cumulative revisions are sent once`() = runBlocking {
        val store = PerformanceStore()
        val reports = mutableListOf<String>()
        val uploader =
            PerformanceUploader({ reports.add(it) }, store::snapshot) {
                wireJson.encodeToString(it)
            }
        uploader.upload()
        assertTrue(reports.isEmpty())
        store.timing(PerfOperation.MARKDOWN, PerfScreen.CHAT, 10_000_000, true)
        uploader.upload()
        uploader.upload()
        assertEquals(1, reports.size)
        store.frame(PerfScreen.CHAT, true, 10_000_000, 16_666_667)
        uploader.upload()
        assertEquals(2, reports.size)
        val oldWindow = store.snapshot().windowId
        store.clear()
        assertNotEquals(oldWindow, store.snapshot().windowId)
        store.timing(PerfOperation.MARKDOWN, PerfScreen.CHAT, 10_000_000, true)
        uploader.upload()
        assertEquals(3, reports.size)
    }

    @Test
    fun `failed uploads are silent and retry the same window without clearing local evidence`() =
        runBlocking {
            val store = PerformanceStore()
            store.frame(PerfScreen.CHAT, true, 900_000_000, 16_666_667)
            var attempts = 0
            val uploader =
                PerformanceUploader(
                    { if (++attempts == 1) throw IOException("Offline") },
                    store::snapshot,
                ) {
                    "report"
                }
            uploader.upload()
            assertEquals(1L, store.snapshot().frames.single().frozen)
            uploader.upload()
            uploader.upload()
            assertEquals(2, attempts)
        }

    @Test
    fun `lifecycle cancellation is propagated so backgrounding or opting out stops uploads`() =
        runBlocking {
            val store = PerformanceStore()
            store.frame(PerfScreen.FIL, false, 10_000_000, 16_666_667)
            val started = CompletableDeferred<Unit>()
            val cancelled = CompletableDeferred<Unit>()
            val uploader =
                PerformanceUploader(
                    {
                        started.complete(Unit)
                        try {
                            awaitCancellation()
                        } finally {
                            cancelled.complete(Unit)
                        }
                    },
                    store::snapshot,
                ) {
                    "report"
                }
            val job = launch { uploader.upload() }
            started.await()
            job.cancelAndJoin()
            assertTrue(cancelled.isCompleted)
            assertTrue(job.isCancelled)
        }

    @Test
    fun `serialization and upload do not run on the calling UI dispatcher`() {
        Executors.newSingleThreadExecutor().asCoroutineDispatcher().use { ui ->
            runBlocking(ui) {
                val caller = Thread.currentThread()
                val store = PerformanceStore()
                store.frame(PerfScreen.CHAT, true, 10_000_000, 16_666_667)
                val uploader =
                    PerformanceUploader(
                        { assertNotSame(caller, Thread.currentThread()) },
                        store::snapshot,
                    ) {
                        assertNotSame(caller, Thread.currentThread())
                        "report"
                    }
                uploader.upload()
                assertSame(caller, Thread.currentThread())
            }
        }
    }
}
