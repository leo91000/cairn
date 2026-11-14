package build.cairn.app.data

import android.content.Context
import android.os.Bundle
import android.os.Parcel
import androidx.compose.runtime.saveable.SaverScope
import androidx.test.core.app.ApplicationProvider
import java.io.File
import java.util.UUID
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

abstract class RuleDraftStoreCases {
    private lateinit var root: File
    private val saverScope =
        object : SaverScope {
            override fun canBeSaved(value: Any) = true
        }

    @Before
    fun createDirectory() {
        root =
            File(
                ApplicationProvider.getApplicationContext<Context>().cacheDir,
                "rule-draft-test-${UUID.randomUUID()}",
            )
        check(root.mkdirs())
    }

    @After
    fun removeDirectory() {
        root.deleteRecursively()
    }

    @Test
    fun largeEditorStateKeepsOnlyAKeyInItsBundleAndRestoresCold() {
        val text = "x".repeat(120_000)
        val state = RuleDraftState(text, text, text)
        val store = RuleDraftStore(root)
        val saver = store.saver("account-a/installation", "editor")
        val token = with(saver) { saverScope.save(state) }!!
        val parcel = Parcel.obtain()
        try {
            parcel.writeBundle(Bundle().apply { putString("draft", token) })
            assertTrue("Saved-state size: ${parcel.dataSize()}", parcel.dataSize() < 4096)
        } finally {
            parcel.recycle()
        }
        assertSame(state, saver.restore(token))
        val restored =
            RuleDraftStore(root).saver("account-a/installation", "editor").restore(token)!!
        assertEquals(text, restored.draft.value)
        assertEquals(text, restored.original.value)
        assertEquals(text, restored.creation.value)
        assertEquals("", restored.problem.value)
        assertFalse(root.listFiles()!!.isEmpty())
    }

    @Test
    fun accountKeysAndDiscardPreventRecoveringAnotherSession() {
        val store = RuleDraftStore(root)
        val state = store.fresh("account-a", "editor").apply { draft.value = "private draft" }
        val saver = store.saver("account-a", "editor")
        val token = with(saver) { saverScope.save(state) }!!
        assertNull(store.saver("account-b", "editor").restore(token))
        store.discard("account-a", "editor", state)
        assertNull(with(saver) { saverScope.save(state) })
        assertTrue(root.listFiles()!!.isEmpty())
    }

    @Test
    fun aFailedDiskWriteStillRetainsTheLiveDraftAndReportsIt() {
        val file = File(root, "not-a-directory").apply { writeText("fixture") }
        val store = RuleDraftStore(file)
        val state = store.fresh("account", "editor").apply { draft.value = "latest changes" }
        val saver = store.saver("account", "editor")
        val token = with(saver) { saverScope.save(state) }!!
        assertEquals("latest changes", saver.restore(token)!!.draft.value)
        assertTrue(state.problem.value.isNotEmpty())
    }

    @Test
    fun clearingTheStoreCannotResurrectAnOldProvider() {
        val store = RuleDraftStore(root)
        val state = store.fresh("account", "editor").apply { draft.value = "discarded" }
        val saver = store.saver("account", "editor")
        val token = with(saver) { saverScope.save(state) }!!
        store.clear()
        assertNull(with(saver) { saverScope.save(state) })
        assertTrue(root.listFiles()!!.isEmpty())
        val fresh = store.fresh("account", "editor")
        assertEquals("", fresh.draft.value)
        assertTrue(token.length < 100)
    }
}
