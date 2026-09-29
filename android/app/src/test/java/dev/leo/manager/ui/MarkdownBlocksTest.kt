package dev.leo.manager.ui

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import io.noties.markwon.Markwon
import io.noties.markwon.ext.strikethrough.StrikethroughPlugin
import io.noties.markwon.ext.tables.TablePlugin
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [36])
class MarkdownBlocksTest {
    private val context = ApplicationProvider.getApplicationContext<Application>()
    private val markwon =
        Markwon.builder(context)
            .usePlugin(TablePlugin.create(context))
            .usePlugin(StrikethroughPlugin.create())
            .build()

    @Test
    fun `append only rerenders the tail and does not rebind completed text views`() {
        val renderer = MarkdownBlocks(markwon)
        val prefix = (1..80).joinToString("\n\n") { "Paragraphe **$it**" }
        val first = renderer.render(prefix + "\n\nRéponse")
        val next = renderer.render(prefix + "\n\nRéponse terminée")
        assertEquals(11, first.size)
        first.dropLast(1).forEachIndexed { i, block -> assertSame(block, next[i]) }
        assertNotSame(first.last(), next.last())
        val view = MarkdownTextView(context)
        view.bind(markwon, first.first())
        val text = view.text
        view.bind(markwon, next.first())
        assertSame(text, view.text)
        assertEquals("Réponse terminée", next.last().text.toString())
    }

    @Test
    fun `late reference definitions invalidate earlier rendered links`() {
        val renderer = MarkdownBlocks(markwon)
        val source = "[Documentation][ref]\n\n" + (1..12).joinToString("\n\n") { "Texte $it" }
        val unresolved = renderer.render(source)
        val resolved = renderer.render(source + "\n\n[ref]: https://example.com/first")
        assertNotSame(unresolved.first(), resolved.first())
        assertTrue(resolved.first().signature.contains("https://example.com/first"))
        val changed = renderer.render(source + "\n\n[ref]: https://example.com/second")
        assertNotSame(resolved.first(), changed.first())
        assertTrue(changed.first().signature.contains("https://example.com/second"))
    }

    @Test
    fun `fences tables lists and corrections retain complete document semantics`() {
        val renderer = MarkdownBlocks(markwon)
        val cases =
            listOf(
                "# Titre\n\n```kotlin\nval a = 1\n\nval b = 2",
                "# Titre\n\n```kotlin\nval a = 1\n\nval b = 2\n```\n\nFin",
                "| Nom | Valeur |\n| :-- | --: |\n| **Un** | ~~Deux~~ |",
                "3. Trois\n4. Quatre\n   - Enfant\n\n> Citation\n> suite",
                "[Lien](https://example.com) avec **gras**, *italique* et `code`.",
                "Texte remplacé",
                "",
            )
        for (source in cases) {
            val blocks = renderer.render(source)
            assertEquals(
                markwon.toMarkdown(source).toString(),
                blocks.joinToString("\n\n") { it.text.toString() },
            )
        }
    }
}
