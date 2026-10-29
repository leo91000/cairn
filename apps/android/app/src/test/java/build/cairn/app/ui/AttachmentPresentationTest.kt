package build.cairn.app.ui

import build.cairn.app.data.ChatAttachment
import org.junit.Assert.*
import org.junit.Test

class AttachmentPresentationTest {
    private fun file(
        name: String,
        size: Long = 1024,
        mediaType: String = "application/octet-stream",
    ) =
        ChatAttachment(
            id = name,
            chatId = "chat",
            name = name,
            size = size.toInt(),
            mediaType = mediaType,
            kind = "file",
        )

    @Test
    fun `files are classified by extension because the server only recognizes images`() {
        assertEquals(AttachmentFamily.PDF, file("Brief_v3_FINAL.PDF").family)
        assertEquals(AttachmentFamily.SHEET, file("export.csv").family)
        assertEquals(AttachmentFamily.SHEET, file("budget.xlsx").family)
        assertEquals(AttachmentFamily.DOC, file("notes.docx").family)
        assertEquals(AttachmentFamily.SLIDES, file("deck.key").family)
        assertEquals(AttachmentFamily.CODE, file("main.rs").family)
        assertEquals(AttachmentFamily.CODE, file("build.gradle").family)
        assertEquals(AttachmentFamily.TEXT, file("README.md").family)
        assertEquals(AttachmentFamily.ARCHIVE, file("backup.tar.gz").family)
        assertEquals(AttachmentFamily.AUDIO, file("memo.m4a").family)
        assertEquals(AttachmentFamily.VIDEO, file("demo.mov").family)
        assertEquals(AttachmentFamily.IMAGE, file("logo.svg").family)
        assertEquals(
            AttachmentFamily.IMAGE,
            ChatAttachment(id = "i", name = "capture", kind = "image").family,
        )
        assertEquals(AttachmentFamily.OTHER, file("data.bin").family)
        // Without a known extension, the media type decides.
        assertEquals(AttachmentFamily.PDF, file("scan", mediaType = "application/pdf").family)
        assertEquals(AttachmentFamily.TEXT, file("notes", mediaType = "text/plain").family)
    }

    @Test
    fun `labels keep a short extension and dotfiles have none`() {
        assertEquals("PDF", attachmentLabel("rapport.pdf"))
        assertEquals("JSON", attachmentLabel("package.jsonl"))
        assertEquals("", attachmentLabel(".env"))
        assertEquals("", attachmentLabel("Makefile"))
        assertEquals("", attachmentLabel("archive.tar-old"))
        assertEquals("", attachmentExtension("name."))
    }

    @Test
    fun `subtitles add page and line counts when the server knows them`() {
        assertEquals("PDF · 812 Ko", attachmentSubtitle(file("brief.pdf", 812 * 1024)))
        assertEquals(
            "PDF · 12 pages · 812 Ko",
            attachmentSubtitle(file("brief.pdf", 812 * 1024), AttachmentSummary(pages = 12)),
        )
        assertEquals(
            "PDF · 1 page · 1 Ko",
            attachmentSubtitle(file("brief.pdf"), AttachmentSummary(pages = 1)),
        )
        assertEquals(
            "CSV · 1 204 lignes · 248 Ko",
            attachmentSubtitle(file("export.csv", 248 * 1024), AttachmentSummary(lines = 1204)),
        )
        assertEquals(
            "ZIP · 1 Ko",
            attachmentSubtitle(file("backup.zip"), AttachmentSummary(lines = 3)),
        )
        assertEquals("Fichier · 1 Ko", attachmentSubtitle(file("Makefile")))
        assertEquals("12 pages", attachmentDetail(file("brief.pdf"), AttachmentSummary(pages = 12)))
        assertNull(attachmentDetail(file("brief.pdf"), null))
    }

    @Test
    fun `document cards picture pdfs and text while other files stay compact`() {
        assertTrue(file("brief.pdf").hasDocumentPreview())
        assertTrue(file("export.tsv").hasDocumentPreview())
        assertTrue(file("main.kt").hasDocumentPreview())
        assertTrue(file("notes.txt").hasDocumentPreview())
        assertFalse(file("budget.xlsx").hasDocumentPreview())
        assertFalse(file("notes.docx").hasDocumentPreview())
        assertFalse(file("backup.zip").hasDocumentPreview())
    }

    @Test
    fun `messages show images first and count those beyond four`() {
        assertEquals(Mosaic(1, 0), mosaic(1))
        assertEquals(Mosaic(3, 0), mosaic(3))
        assertEquals(Mosaic(4, 0), mosaic(4))
        assertEquals(Mosaic(4, 3), mosaic(7))
        val zip = file("backup.zip")
        val pdf = file("brief.pdf")
        val photo = ChatAttachment(id = "p", chatId = "chat", name = "photo.jpg", kind = "image")
        assertEquals(listOf(photo, pdf, zip), displayOrder(listOf(zip, pdf, photo)))
    }
}
