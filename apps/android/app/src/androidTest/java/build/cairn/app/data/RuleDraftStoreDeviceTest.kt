package build.cairn.app.data

import android.content.Context
import androidx.compose.runtime.saveable.SaverScope
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import java.io.File
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RuleDraftStoreDeviceTest : RuleDraftStoreCases() {
    private val scope =
        object : SaverScope {
            override fun canBeSaved(value: Any) = true
        }

    private fun encryptedCase(action: (Context, File) -> Unit) {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val root = File(context.noBackupFilesDir, "encrypted-draft-test-${UUID.randomUUID()}")
        try {
            action(context, root)
        } finally {
            root.deleteRecursively()
        }
    }

    @Test
    fun keystoreCodecRestoresAnEncryptedDraftThroughANewStore() = encryptedCase { context, root ->
        val text = "private text ".repeat(10_000)
        val state = RuleDraftState(text, text, text)
        val saver = RuleDraftStore.encrypted(context, root).saver("account-a/install", "editor")
        val token = with(saver) { scope.save(state) }!!
        assertEquals("", state.problem.value)
        assertFalse(File(root, token).readBytes().decodeToString().contains("private text"))
        val restored =
            RuleDraftStore.encrypted(context, root)
                .saver("account-a/install", "editor")
                .restore(token)!!
        assertEquals(text, restored.draft.value)
        assertEquals(text, restored.original.value)
        assertEquals(text, restored.creation.value)
        assertEquals("", restored.problem.value)
    }

    @Test
    fun encryptedFilesCannotBeMovedToAnotherAccountScope() = encryptedCase { context, root ->
        val store = RuleDraftStore.encrypted(context, root)
        val first = store.saver("account-a/install", "editor")
        val tokenA = with(first) { scope.save(RuleDraftState("private")) }!!
        val second = store.saver("account-b/install", "editor")
        val tokenB = with(second) { scope.save(RuleDraftState("other")) }!!
        assertNull(second.restore(tokenA))
        File(root, tokenB).writeBytes(File(root, tokenA).readBytes())
        val restored =
            RuleDraftStore.encrypted(context, root)
                .saver("account-b/install", "editor")
                .restore(tokenB)!!
        assertEquals("", restored.draft.value)
        assertTrue(restored.problem.value.isNotEmpty())
    }

    @Test
    fun aCorruptEncryptedDraftReportsLossInsteadOfReturningPartialText() =
        encryptedCase { context, root ->
            val saver = RuleDraftStore.encrypted(context, root).saver("account/install", "editor")
            val token = with(saver) { scope.save(RuleDraftState("draft")) }!!
            File(root, token).writeBytes(byteArrayOf(1, 2, 3))
            val restored =
                RuleDraftStore.encrypted(context, root)
                    .saver("account/install", "editor")
                    .restore(token)!!
            assertEquals("", restored.draft.value)
            assertTrue(restored.problem.value.isNotEmpty())
        }
}
