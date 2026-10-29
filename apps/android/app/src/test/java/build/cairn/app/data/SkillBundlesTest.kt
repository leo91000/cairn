package build.cairn.app.data

import java.util.Base64
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

class SkillBundlesTest {
    private val bundle =
        SkillBundle(
            scope = "global",
            name = "review",
            revision = "rev-1",
            files =
                listOf(
                    SkillFile(path = "SKILL.md", size = 40),
                    SkillFile(path = "scripts/check.sh", size = 20, executable = true),
                    SkillFile(path = "references/guide.md", size = 30),
                    SkillFile(path = "assets/logo.png", size = 50, text = false),
                    SkillFile(path = "old.md", size = 10),
                ),
            limits = SkillLimits(),
        )
    private val skill = "---\nname: review\ndescription: Relire\n---\n\n# Review\n"

    private fun draft() = SkillDraft.of(bundle).withOriginal("SKILL.md", SkillBody.Text(skill))

    @Test
    fun invalidSkillsCanBeDecodedAndRepaired() {
        val decoded =
            wireJson.decodeFromString<SkillBundle>(
                """{"name":"review","description":null,"files":[{"path":"SKILL.md","size":3}]}"""
            )
        assertTrue(SkillDraft.of(decoded).edit(SKILL_FILE, skill).problems().isEmpty())
    }

    @Test
    fun yamlCommentsDoNotPreventSavingValidSkills() {
        val commented = skill.replace("name: review", "name: review # reusable")
        assertTrue(draft().edit(SKILL_FILE, commented).problems().isEmpty())
        val quoted = skill.replace("name: review", "name: 'review' # reusable")
        assertTrue(draft().edit(SKILL_FILE, quoted).problems().isEmpty())
    }

    @Test
    fun legacySizesAllowEditsWithoutFurtherGrowth() {
        val legacy =
            bundle.copy(
                limits = SkillLimits(fileBytes = 100, bundleBytes = 200, files = 2),
                files =
                    listOf(
                        SkillFile(path = SKILL_FILE, size = 50),
                        SkillFile(path = "large.bin", size = 1000),
                        SkillFile(path = "old", size = 10),
                    ),
            )
        val edited = SkillDraft.of(legacy).edit(SKILL_FILE, skill).delete("old")
        assertTrue(edited.problems().isEmpty())
        assertTrue(edited.put("large.bin", SkillBody.Text("x".repeat(101))).problems().isNotEmpty())
        assertTrue(edited.put("extra", SkillBody.Text("x".repeat(100))).problems().isNotEmpty())
    }

    @Test
    fun maximumLengthPathsCanBeSwappedThroughValidTemporaryNames() {
        val a = "a".repeat(240)
        val b = "b".repeat(240)
        val saved =
            bundle.copy(
                files =
                    listOf(SkillFile(path = SKILL_FILE), SkillFile(path = a), SkillFile(path = b))
            )
        val swapped = SkillDraft.of(saved).move(a, "parking").move(b, a).move("parking", b)
        val occupied = mutableSetOf(SKILL_FILE, a, b)
        for (move in swapped.changes().filterIsInstance<DraftSkillChange.Move>()) {
            assertNull(skillPathProblem(move.to))
            assertFalse(occupied.any { pathsCollide(move.to, it) })
            assertTrue(occupied.remove(move.from))
            occupied += move.to
        }
        assertEquals(setOf(SKILL_FILE, a, b), occupied)
    }

    @Test
    fun treeKeepsSkillFirstThenFoldersThenFilesAndHidesCollapsedChildren() {
        val rows =
            skillTree(
                listOf("b.md", "SKILL.md", "scripts/run.py", "scripts/lib/util.sh", "A.md"),
                folders = setOf("empty"),
                deleted = listOf("scripts/old.sh"),
            )
        assertEquals(
            listOf(
                "SKILL.md",
                "empty",
                "scripts",
                "scripts/lib",
                "scripts/lib/util.sh",
                "scripts/old.sh",
                "scripts/run.py",
                "A.md",
                "b.md",
            ),
            rows.map { it.path },
        )
        assertEquals(listOf(0, 0, 0, 1, 2, 1, 1, 0, 0), rows.map { it.depth })
        assertTrue(rows.single { it.path == "scripts/old.sh" }.deleted)
        assertTrue(rows.single { it.path == "scripts/lib" }.folder)

        val collapsed =
            skillTree(
                listOf("SKILL.md", "scripts/run.py", "scripts/lib/util.sh"),
                collapsed = setOf("scripts"),
            )
        assertEquals(listOf("SKILL.md", "scripts"), collapsed.map { it.path })
    }

    @Test
    fun unchangedDraftHasNoChangesAndRevertedEditsAreNotDirty() {
        val draft = draft()
        assertFalse(draft.dirty)
        val edited = draft.edit("SKILL.md", skill + "More\n")
        assertTrue(edited.dirty)
        assertEquals(DraftStatus.MODIFIED, edited.status(edited.file("SKILL.md")!!))
        val reverted = edited.edit("SKILL.md", skill)
        assertFalse(reverted.dirty)
        assertNull(reverted.file("SKILL.md")!!.body)
    }

    @Test
    fun changesAreOrderedDeletesMovesWritesThenModes() {
        val draft =
            draft()
                .delete("old.md")
                .move("references/guide.md", "docs/guide.md")
                .edit("docs/guide.md", "# Guide\n")
                .put("scripts/new.py", SkillBody.Text("#!/usr/bin/env python3\nprint(1)\n"))
                .setExecutable("scripts/check.sh", false)
                .put("assets/logo.png", SkillBody.Binary(byteArrayOf(1, 2, 0, 3)))
        val changes = draft.changes()
        assertEquals(
            listOf(
                DraftSkillChange.Delete("old.md"),
                DraftSkillChange.Move("references/guide.md", "docs/guide.md"),
                DraftSkillChange.Write("docs/guide.md", SkillBody.Text("# Guide\n"), false),
                DraftSkillChange.Write(
                    "assets/logo.png",
                    SkillBody.Binary(byteArrayOf(1, 2, 0, 3)),
                    false,
                ),
                DraftSkillChange.Write(
                    "scripts/new.py",
                    SkillBody.Text("#!/usr/bin/env python3\nprint(1)\n"),
                    true,
                ),
                DraftSkillChange.Mode("scripts/check.sh", false),
            ),
            changes,
        )
        assertEquals(listOf("old.md"), draft.deleted.map { it.path })
        assertEquals(DraftStatus.NEW, draft.status(draft.file("scripts/new.py")!!))
        assertEquals(DraftStatus.MODIFIED, draft.status(draft.file("scripts/check.sh")!!))
    }

    @Test
    fun movesAreChainedAndSwapsUseATemporaryPath() {
        val chained = draft().move("old.md", "older.md").move("references/guide.md", "old.md")
        assertEquals(
            listOf(
                DraftSkillChange.Move("old.md", "older.md"),
                DraftSkillChange.Move("references/guide.md", "old.md"),
            ),
            chained.changes(),
        )

        val swapped =
            draft()
                .move("old.md", "swap.md")
                .move("references/guide.md", "old.md")
                .move("swap.md", "references/guide.md")
        val moves = swapped.changes().filterIsInstance<DraftSkillChange.Move>()
        assertEquals(3, moves.size)
        // Replaying the batch on the saved paths produces the expected final layout.
        val paths = bundle.files.map { it.path }.toMutableSet()
        val contents = bundle.files.associate { it.path to it.path }.toMutableMap()
        for (move in moves) {
            assertTrue("${move.from} exists", move.from in paths)
            assertFalse("${move.to} is free", move.to in paths)
            paths -= move.from
            paths += move.to
            contents[move.to] = contents.remove(move.from)!!
        }
        assertEquals("references/guide.md", contents["old.md"])
        assertEquals("old.md", contents["references/guide.md"])
    }

    @Test
    fun movesWaitWhileAParentFolderOfTheTargetIsStillAFile() {
        val base =
            SkillBundle(
                scope = "global",
                name = "review",
                revision = "rev-1",
                // alpha/x comes first, so only the parent-folder check keeps it waiting.
                files =
                    listOf(
                        SkillFile(path = "SKILL.md", size = 10),
                        SkillFile(path = "alpha/x", size = 4),
                        SkillFile(path = "zeta", size = 3),
                    ),
                limits = SkillLimits(),
            )
        val draft = SkillDraft.of(base).move("zeta", "zeta.md").moveFolder("alpha", "zeta")
        assertEquals(
            listOf(
                DraftSkillChange.Move("zeta", "zeta.md"),
                DraftSkillChange.Move("alpha/x", "zeta/x"),
            ),
            draft.changes(),
        )

        // The reverse: a file takes the path of a folder whose file moves away later.
        val reverse =
            SkillDraft.of(
                    base.copy(
                        files =
                            listOf(
                                SkillFile(path = "SKILL.md", size = 10),
                                SkillFile(path = "lib.md", size = 4),
                                SkillFile(path = "lib/a.sh", size = 3),
                            )
                    )
                )
                .move("lib.md", "notes.md")
                .move("lib/a.sh", "scripts/a.sh")
                .move("notes.md", "lib")
        assertEquals(
            listOf(
                DraftSkillChange.Move("lib/a.sh", "scripts/a.sh"),
                DraftSkillChange.Move("lib.md", "lib"),
            ),
            reverse.changes(),
        )

        // Mutually blocking folder/file moves still resolve through a temporary path.
        val crossed =
            SkillDraft.of(
                    base.copy(
                        files =
                            listOf(
                                SkillFile(path = "SKILL.md", size = 10),
                                SkillFile(path = "a", size = 1),
                                SkillFile(path = "b", size = 1),
                            )
                    )
                )
                .move("a", "c")
                .move("b", "a/y")
                .move("c", "b/x")
        val moves = crossed.changes().filterIsInstance<DraftSkillChange.Move>()
        val files = mutableSetOf("SKILL.md", "a", "b")
        for (move in moves) {
            assertTrue(move.from in files)
            assertFalse(files.any { pathsCollide(move.to, it) })
            files -= move.from
            files += move.to
        }
        assertEquals(setOf("SKILL.md", "a/y", "b/x"), files)
    }

    @Test
    fun foldersMoveTheirFilesAndDeletedFilesCanBeRestored() {
        val moved = draft().moveFolder("scripts", "tools/scripts")
        assertEquals(
            listOf(DraftSkillChange.Move("scripts/check.sh", "tools/scripts/check.sh")),
            moved.changes(),
        )
        assertEquals(DraftStatus.MOVED, moved.status(moved.file("tools/scripts/check.sh")!!))
        val emptied = draft().deleteFolder("references")
        assertEquals(listOf(DraftSkillChange.Delete("references/guide.md")), emptied.changes())
        assertFalse(emptied.restore("references/guide.md").dirty)

        val withFolder = draft().addFolder("templates")
        assertTrue("templates" in withFolder.folderPaths())
        // An empty folder alone does not need a save.
        assertFalse(withFolder.dirty)
    }

    @Test
    fun editsRefuseInvalidOrTakenPaths() {
        val draft = draft()
        for (path in
            listOf(
                ".env",
                "a/.hidden",
                "/abs",
                "a//b",
                "a/b/c/d/e/f/g/h/i",
                "x".repeat(241),
            )) assertTrue(path, skillPathProblem(path) != null)
        assertNull(skillPathProblem("references/guide (v2).md"))
        assertThrows { draft.move("old.md", "SKILL.md") }
        assertThrows { draft.move("old.md", "scripts") }
        assertThrows { draft.move("SKILL.md", "docs/SKILL.md") }
        assertThrows { draft.delete("SKILL.md") }
        assertThrows { draft.put(".secrets", SkillBody.Text("x")) }
        assertThrows { draft.moveFolder("scripts", "scripts/inner") }
    }

    @Test
    fun fileAncestorsAndDescendantFoldersCannotCollideInTheDraft() {
        val saved =
            bundle.copy(
                files =
                    listOf(
                        SkillFile(path = SKILL_FILE),
                        SkillFile(path = "notes"),
                        SkillFile(path = "source/a.txt"),
                    )
            )
        val draft = SkillDraft.of(saved)
        assertThrows { draft.put("notes/child.txt", SkillBody.Text("x")) }
        assertThrows { draft.addFolder("notes/nested") }
        assertThrows { draft.move("source/a.txt", "notes/a.txt") }
        assertThrows { draft.moveFolder("source", "notes/moved") }
        assertThrows {
            draft.delete("notes").put("notes/child.txt", SkillBody.Text("x")).restore("notes")
        }

        val nested = draft.move("notes", "notes/index.txt")
        val occupied = mutableSetOf(SKILL_FILE, "notes", "source/a.txt")
        for (move in nested.changes().filterIsInstance<DraftSkillChange.Move>()) {
            assertFalse(occupied.any { pathsCollide(move.to, it) })
            assertTrue(occupied.remove(move.from))
            occupied += move.to
        }
        assertTrue("notes/index.txt" in occupied)
    }

    @Test
    fun problemsFollowTheServerLimitsAndLeaveYamlValidationToTheServer() {
        assertEquals(emptyList<String>(), draft().problems())
        val renamed = draft().edit("SKILL.md", "---\nname: other\n---\n")
        assertTrue(renamed.problems().isEmpty())
        val large =
            draft()
                .copy(
                    base =
                        bundle.copy(
                            limits = SkillLimits(fileBytes = 4, bundleBytes = 100, files = 3)
                        )
                )
                .put("big.txt", SkillBody.Text("12345"))
        val problems = large.problems()
        assertTrue(problems.any { it.startsWith("big.txt dépasse") })
        assertTrue(
            large.put("extra", SkillBody.Text("x")).problems().any {
                it.startsWith("Un skill compte au plus 3")
            }
        )
        assertTrue(problems.any { it.startsWith("Le skill dépasse") })
        val created = SkillDraft.create("my-skill")
        assertTrue(created.dirty)
        assertEquals(emptyList<String>(), created.problems())
        assertEquals(
            "my-skill",
            skillFrontmatter((created.content("SKILL.md") as SkillBody.Text).value)["name"],
        )
    }

    @Test
    fun changesSerializeToTheBatchFormat() {
        val write =
            DraftSkillChange.Write(
                    "a.bin",
                    SkillBody.Binary(byteArrayOf(0, 1, 2)),
                    executable = null,
                )
                .toJson()
        assertEquals("write", write["op"]!!.jsonPrimitive.content)
        assertEquals("base64", write["encoding"]!!.jsonPrimitive.content)
        assertEquals("AAEC", write["content"]!!.jsonPrimitive.content)
        assertFalse("executable" in write)
        val staged =
            DraftSkillChange.Write("big.md", SkillBody.Text("x"), true, upload = "up-1").toJson()
        assertEquals("up-1", staged["upload"]!!.jsonPrimitive.content)
        assertFalse("content" in staged)
        assertEquals("true", staged["executable"]!!.jsonPrimitive.content)
        assertEquals(
            """{"op":"move","from":"a","to":"b"}""",
            DraftSkillChange.Move("a", "b").toJson().toString(),
        )
        assertEquals(
            """{"op":"mode","path":"a","executable":true}""",
            DraftSkillChange.Mode("a", true).toJson().toString(),
        )
    }

    @Test
    fun binariesAndOverflowingTextAreStagedToKeepTheBatchSmall() {
        val small = DraftSkillChange.Write("small.md", SkillBody.Text("x".repeat(1000)))
        val icon = DraftSkillChange.Write("icon.png", SkillBody.Binary(ByteArray(10)))
        assertEquals(setOf("icon.png"), stagedWrites(listOf(small, icon)))
        val texts =
            (1..4).map {
                DraftSkillChange.Write("t$it.md", SkillBody.Text("y".repeat(40_000 + it)))
            }
        val changes = listOf(DraftSkillChange.Delete("old.md")) + texts + small
        val staged = stagedWrites(changes)
        // The largest texts go first; the small ones stay inline.
        assertEquals(setOf("t4.md", "t3.md"), staged)
        val inline =
            changes
                .filter { it !is DraftSkillChange.Write || it.path !in staged }
                .sumOf { inlineCost(it) }
        assertTrue(inline <= SKILL_REQUEST_BUDGET)
        val skill = DraftSkillChange.Write("SKILL.md", SkillBody.Text("z".repeat(100_000)))
        assertEquals(emptySet<String>(), stagedWrites(listOf(skill)))
    }

    @Test
    fun textWithLongJsonEscapesIsStagedBeforeItExceedsTheRequestLimit() {
        val controls =
            DraftSkillChange.Write("controls.txt", SkillBody.Text("\u0001".repeat(30_000)))
        assertEquals(setOf("controls.txt"), stagedWrites(listOf(controls)))
        assertEquals(
            controls.toJson().toString().toByteArray(Charsets.UTF_8).size.toLong(),
            inlineCost(controls),
        )
    }

    @Test
    fun bodiesDetectTextLikeTheServer() {
        assertTrue(SkillBody.of("héllo".toByteArray()) is SkillBody.Text)
        assertTrue(SkillBody.of(byteArrayOf(0x68, 0)) is SkillBody.Binary)
        assertTrue(SkillBody.of(byteArrayOf(0xC3.toByte(), 0x28)) is SkillBody.Binary)
        val content =
            SkillFileContent(
                content = Base64.getEncoder().encodeToString(byteArrayOf(9, 0)),
                encoding = "base64",
                size = 2,
            )
        assertEquals(SkillBody.Binary(byteArrayOf(9, 0)), SkillBody.of(content))
    }

    @Test
    fun archiveDownloadFollowsOffsetsWithTheFirstRevision() = runTest {
        val archive = ByteArray(10) { it.toByte() }
        val requests = mutableListOf<Pair<Long, String?>>()
        val written = java.io.ByteArrayOutputStream()
        val progress = mutableListOf<Long>()
        val result =
            downloadSkillArchive(
                { offset, revision ->
                    requests += offset to revision
                    val end = minOf(archive.size.toLong(), offset + 4).toInt()
                    SkillArchiveChunk(
                        filename = "review.zip",
                        revision = "rev-1",
                        size = archive.size.toLong(),
                        offset = offset,
                        content =
                            Base64.getEncoder()
                                .encodeToString(archive.copyOfRange(offset.toInt(), end)),
                    )
                },
                { written.write(it) },
            ) { done, _ ->
                progress += done
            }
        assertEquals(listOf(0L to null, 4L to "rev-1", 8L to "rev-1"), requests)
        assertEquals(archive.toList(), written.toByteArray().toList())
        assertEquals(listOf(4L, 8L, 10L), progress)
        assertEquals("review.zip", result.filename)
        assertEquals(10L, result.size)
    }

    @Test
    fun archiveDownloadStopsWhenTheRevisionChangesOrDataIsMissing() = runTest {
        var calls = 0
        try {
            downloadSkillArchive(
                { offset, _ ->
                    calls++
                    SkillArchiveChunk(
                        filename = "r.zip",
                        revision = "rev-$calls",
                        size = 8,
                        offset = offset,
                        content = Base64.getEncoder().encodeToString(ByteArray(4)),
                    )
                },
                {},
            )
            fail("A changed revision must stop the download.")
        } catch (e: IllegalStateException) {
            assertEquals("Le skill a changé pendant le téléchargement. Réessayez.", e.message)
        }
        try {
            downloadSkillArchive(
                { offset, _ ->
                    SkillArchiveChunk(
                        filename = "r.zip",
                        revision = "rev",
                        size = 8,
                        offset = offset,
                        content = "",
                    )
                },
                {},
            )
            fail("An empty chunk before the end must stop the download.")
        } catch (e: IllegalStateException) {
            assertEquals("L’archive reçue est incomplète.", e.message)
        }
        val empty =
            downloadSkillArchive(
                { _, _ ->
                    SkillArchiveChunk(
                        filename = "e.zip",
                        revision = "rev",
                        size = 0,
                        offset = 0,
                        content = "",
                    )
                },
                {},
            )
        assertEquals(0L, empty.size)
    }

    @Test
    fun pluginSourcesAreReadAndImportedGenerically() {
        val skill =
            wireJson.decodeFromString<Skill>(
                """{"name":"pdf","source":{"pluginId":"github","source":"repository","instance":"acct","label":"anthropics/skills","location":{"repository":"anthropics/skills","ref":"main"},"url":"https://github.com/anthropics/skills/tree/9b1e2c4d5/skills/pdf","folder":"skills/pdf","version":"9b1e2c4d5e6f","importedAt":1,"modified":true,"later":1}}"""
            )
        val source = skill.source!!
        assertEquals("anthropics/skills", source.label)
        assertEquals("skills/pdf", source.folder)
        assertEquals("9b1e2c4", source.shortVersion)
        assertEquals("anthropics/skills@9b1e2c4", source.reference)
        assertEquals("https://github.com/anthropics/skills/tree/9b1e2c4d5/skills/pdf", source.url)
        assertTrue(source.modified)
        // A source without a version or label still names something.
        assertEquals(
            "forge",
            SkillSource(
                    pluginId = "forge",
                    location = kotlinx.serialization.json.JsonObject(emptyMap()),
                )
                .reference,
        )

        val status =
            wireJson.decodeFromString<SkillSourceStatus>(
                """{"source":{"pluginId":"github","source":"repository","label":"o/r","location":{},"folder":"","version":"aaa","importedAt":1,"modified":false},"latest":{"version":"bbbbbbbbb","message":"Fix","date":"2026-09-01T00:00:00Z"},"current":false,"changes":[]}"""
            )
        assertEquals("bbbbbbb", shortVersion(status.latest.version))
        assertFalse(status.current)

        val request =
            build.cairn.contracts.ImportSkillInput.B(
                pluginId = "github",
                skillSource = "repository",
                scope = "global",
                location = buildJsonObject { put("repository", "o/r") },
                folder = "skills/pdf",
            )
        val body =
            build.cairn.contracts.contractJson
                .encodeToJsonElement<build.cairn.contracts.ImportSkillInput>(request)
                .jsonObject
        assertEquals("plugin", body["source"]!!.jsonPrimitive.content)
        assertEquals("github", body["pluginId"]!!.jsonPrimitive.content)
        assertEquals("global", body["scope"]!!.jsonPrimitive.content)
        assertFalse("instance" in body)
        assertEquals("skills/pdf", body["folder"]!!.jsonPrimitive.content)
        val chosen =
            build.cairn.contracts.contractJson
                .encodeToJsonElement<build.cairn.contracts.ImportSkillInput>(
                    request.copy(instance = "acct", folder = null)
                )
                .jsonObject
        assertEquals("acct", chosen["instance"]!!.jsonPrimitive.content)
        assertFalse("folder" in chosen)
    }

    @Test
    fun previewsSplitTheFrontmatterFromTheMarkdown() {
        val markdown = "---\nname: review\ndescription: \"Relire: tout\"\n---\n\n# Titre\n"
        assertEquals(
            mapOf("name" to "review", "description" to "Relire: tout"),
            skillFrontmatter(markdown),
        )
        assertEquals("# Titre\n", skillMarkdownBody(markdown))
        assertEquals("# Sans en-tête\n", skillMarkdownBody("# Sans en-tête\n"))
        assertEquals(emptyMap<String, String>(), skillFrontmatter("---\nname: x\n"))
    }

    @Test
    fun fileKindsFollowPathAndMode() {
        assertEquals(SkillFileKind.SKILL, skillFileKind("SKILL.md", true, false))
        assertEquals(SkillFileKind.MARKDOWN, skillFileKind("references/a.md", true, false))
        assertEquals(SkillFileKind.SCRIPT, skillFileKind("scripts/run", true, true))
        assertEquals(SkillFileKind.SCRIPT, skillFileKind("scripts/run.py", true, false))
        assertEquals(SkillFileKind.IMAGE, skillFileKind("assets/logo.PNG", false, false))
        assertEquals(SkillFileKind.BINARY, skillFileKind("assets/font.woff2", false, false))
        assertEquals(SkillFileKind.TEXT, skillFileKind("templates/a.json", true, false))
        assertEquals(
            "o/r@9b1e2c4",
            SkillSource(
                    label = "o/r",
                    version = "9b1e2c4d5",
                    location = kotlinx.serialization.json.JsonObject(emptyMap()),
                )
                .reference,
        )
    }

    @Test
    fun skillRequestsUseTheLongTimeout() {
        assertTrue(longRequest("/skills/global/review/bundle"))
        assertTrue(longRequest("/skills/uploads"))
        assertTrue(longRequest("/skills/global/import"))
        assertTrue(longRequest("/skills/p/review/source/update"))
        assertTrue(longRequest("/projects/import"))
        assertFalse(longRequest("/projects"))
        assertFalse(longRequest("/skills/global/review/file?path=bundle"))
    }

    @Test
    fun batchesStageBinariesAsRawUploadsThenSendOneRequest() = runTest {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("""{"id":"up-1","size":4}"""))
            server.enqueue(
                MockResponse()
                    .setBody("""{"scope":"global","name":"review","revision":"rev-2","files":[]}""")
            )
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
            val bundle =
                api.applyDraftSkillChanges(
                    "global",
                    "review",
                    "rev-1",
                    listOf(
                        DraftSkillChange.Delete("old.md"),
                        DraftSkillChange.Write(
                            "assets/logo.png",
                            SkillBody.Binary(byteArrayOf(1, 2, 0, 3)),
                            false,
                        ),
                        DraftSkillChange.Write("notes.md", SkillBody.Text("hi"), false),
                    ),
                )
            assertEquals("rev-2", bundle.revision)
            val upload = server.takeRequest()
            assertEquals("POST", upload.method)
            assertEquals("/api/installations/i/api/skills/uploads", upload.path)
            assertEquals("application/octet-stream", upload.getHeader("Content-Type"))
            assertEquals(listOf<Byte>(1, 2, 0, 3), upload.body.readByteArray().toList())
            val batch = server.takeRequest()
            assertEquals("/api/installations/i/api/skills/global/review/bundle", batch.path)
            val json = wireJson.parseToJsonElement(batch.body.readUtf8()).jsonObject
            assertEquals("rev-1", json["base"]!!.jsonPrimitive.content)
            assertEquals(
                """[{"op":"delete","path":"old.md"},{"op":"write","path":"assets/logo.png","upload":"up-1","executable":false},{"op":"write","path":"notes.md","content":"hi","executable":false}]""",
                json["changes"].toString(),
            )
        }
    }

    @Test
    fun newSkillsSendANullBase() = runTest {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse().setBody("""{"scope":"p1","name":"notes","revision":"r"}""")
            )
            server.start()
            val api = CairnApi(server.url("/"), MemoryVault(), installationId = "i")
            api.applyDraftSkillChanges("p1", "notes", null, SkillDraft.create("notes").changes())
            val json = wireJson.parseToJsonElement(server.takeRequest().body.readUtf8()).jsonObject
            assertEquals("null", json["base"].toString())
        }
    }

    private fun assertThrows(block: () -> Unit) {
        try {
            block()
            fail("Expected a refused edit.")
        } catch (_: IllegalArgumentException) {} catch (_: IllegalStateException) {}
    }
}
