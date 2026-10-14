package build.cairn.app.ui

import android.app.Application
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.systemBarsPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.test.core.app.ApplicationProvider
import build.cairn.app.data.*
import java.util.Base64
import java.util.concurrent.CopyOnWriteArrayList
import kotlinx.coroutines.flow.first
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test

/**
 * Skill bundles against a local fixture API: library cards, the workbench tree and editors, one
 * batched save, conflicts, GitHub source updates, imports and read-only members.
 */
abstract class SkillWorkbenchCases {
    protected open fun capture(name: String) {}

    @get:Rule val compose = createComposeRule()

    private val api = "/api/installations/fixture/api"

    private val skillMarkdown =
        """
        ---
        name: review
        description: Relire une branche, lancer les vérifications et rédiger un compte rendu clair.
        ---

        # Revue de code

        1. Lire `references/guide.md` pour les conventions.
        2. Lancer `scripts/check.sh` depuis la racine du dépôt.
        3. Résumer les risques et les suites à donner.
        """
            .trimIndent() + "\n"

    private val checkScript =
        """
        #!/bin/sh
        set -eu
        # Vérifie le format puis lance la suite de tests.
        cargo fmt --all --check
        cargo test --workspace --quiet
        echo "Revue prête"
        """
            .trimIndent() + "\n"

    private val lintScript =
        """
        import subprocess
        import sys

        result = subprocess.run(["ruff", "check", "."], check=False)
        sys.exit(result.returncode)
        """
            .trimIndent() + "\n"

    private val guide = "# Guide\n\nPréférez des commits courts et des messages explicites.\n"
    private val logo: ByteArray = Base64.getDecoder().decode(CAIRN_PNG)

    private val contents =
        mapOf(
            "SKILL.md" to skillMarkdown.toByteArray(),
            "scripts/check.sh" to checkScript.toByteArray(),
            "scripts/lint.py" to lintScript.toByteArray(),
            "references/guide.md" to guide.toByteArray(),
            "assets/logo.png" to logo,
            "old.md" to "Ancienne note\n".toByteArray(),
        )

    private fun files(paths: Map<String, ByteArray>, executables: Set<String>) =
        paths.map { (path, bytes) ->
            SkillFile(path, bytes.size.toLong(), path in executables, path != "assets/logo.png")
        }

    private val reviewBundle =
        SkillBundle(
            "global",
            "review",
            "4f1c0d2e9a7b",
            files(contents, setOf("scripts/check.sh")),
        )

    private val pdfSource =
        SkillSource(
            repository = "anthropics/skills",
            path = "skills/pdf",
            ref = "main",
            commit = "9b1e2c4d5f60718293a4",
            importedAt = 1791976037271,
            modified = true,
        )

    private val pdfBundle =
        SkillBundle(
            "global",
            "pdf-tools",
            "77aa01",
            listOf(
                SkillFile("SKILL.md", 120),
                SkillFile("scripts/extract.py", 640, executable = true),
                SkillFile("reference.md", 2048),
            ),
            source = pdfSource,
        )

    private val skills =
        listOf(
            Skill(
                "review",
                "Relire une branche, lancer les vérifications et rédiger un compte rendu clair.",
                files = 6,
                bytes = contents.values.sumOf { it.size }.toLong(),
                executables = 1,
            ),
            Skill(
                "pdf-tools",
                "Extraire du texte et des tableaux depuis des PDF.",
                files = 3,
                bytes = 2808,
                executables = 1,
                source = pdfSource,
            ),
            Skill(
                "release-notes",
                "Rédiger les notes de version à partir des commits.",
                scope = "project",
                files = 1,
                bytes = 900,
            ),
        )

    private class Fixture {
        val batches = CopyOnWriteArrayList<Pair<String, JsonObject>>()
        var saveGate: java.util.concurrent.CountDownLatch? = null
        @Volatile var saveReturned = false
        var deleteGate: java.util.concurrent.CountDownLatch? = null
        @Volatile var deleteStarted = false
        @Volatile var deleteReturned = false
        val referenceReads = java.util.concurrent.atomic.AtomicInteger()
        var oldReferenceGate: java.util.concurrent.CountDownLatch? = null
        var newReferenceGate: java.util.concurrent.CountDownLatch? = null
        @Volatile var oldReferenceReturned = false
        @Volatile var referenceUpdated = false
        val imports = CopyOnWriteArrayList<JsonObject>()
        val updates = CopyOnWriteArrayList<JsonObject>()
        @Volatile var conflict = false
        @Volatile var bundleLoads = 0
    }

    private fun skillDispatcher(fixture: Fixture, role: String) =
        object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val path = request.path!!.substringBefore('?')
                val query = request.requestUrl?.queryParameter("path")
                fun json(value: String) = MockResponse().setBody(value)
                return when {
                    path == "/api/installations" ->
                        json(
                            """[{"id":"fixture","name":"Installation de test","role":"$role","online":true}]"""
                        )
                    path == "/api/account/session" ->
                        json(
                            """{"authenticated":true,"csrf":"fixture","account":{"id":"a","email":"owner@example.test"},"installations":[{"id":"fixture","name":"Installation de test","role":"$role","online":true}]}"""
                        )
                    path == "$api/skills" -> json(wireJson.encodeToString(skills))
                    path == "$api/skills/global/review" && request.method == "DELETE" -> {
                        fixture.deleteStarted = true
                        fixture.deleteGate?.await(60, java.util.concurrent.TimeUnit.SECONDS)
                        fixture.deleteReturned = true
                        json("""{"deleted":true}""")
                    }
                    path == "$api/projects" ->
                        json(wireJson.encodeToString(listOf(Project("project", "Cairn"))))
                    path == "$api/skills/global/review/bundle" && request.method == "GET" -> {
                        fixture.bundleLoads++
                        json(wireJson.encodeToString(reviewBundle))
                    }
                    path == "$api/skills/global/review/file" -> {
                        val bytes = contents.getValue(query!!)
                        val binary = query == "assets/logo.png"
                        json(
                            wireJson.encodeToString(
                                SkillFileContent(
                                    if (binary) Base64.getEncoder().encodeToString(bytes)
                                    else String(bytes),
                                    if (binary) "base64" else "utf-8",
                                    bytes.size.toLong(),
                                    query == "scripts/check.sh",
                                )
                            )
                        )
                    }
                    path.endsWith("/bundle") && request.method == "POST" -> {
                        val body = wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                        fixture.batches += path to body
                        fixture.saveGate?.await(60, java.util.concurrent.TimeUnit.SECONDS)
                        fixture.saveReturned = true
                        if (fixture.conflict)
                            MockResponse()
                                .setResponseCode(409)
                                .setBody("""{"error":"Le skill a changé."}""")
                        else
                            json(
                                wireJson.encodeToString(
                                    if (path.contains("/review/"))
                                        reviewBundle.copy(
                                            revision = "rev-2",
                                            files =
                                                files(
                                                    contents - "old.md" - "references/guide.md" +
                                                        ("docs/guide.md" to guide.toByteArray()) +
                                                        ("references/checklist.md" to
                                                            "- [ ] Tests\n".toByteArray()),
                                                    setOf("scripts/check.sh", "scripts/lint.py"),
                                                ),
                                        )
                                    else
                                        SkillBundle(
                                            "global",
                                            "release-checklist",
                                            "new-1",
                                            listOf(SkillFile("SKILL.md", 140)),
                                        )
                                )
                            )
                    }
                    path == "$api/skills/global/pdf-tools/bundle" ->
                        json(wireJson.encodeToString(pdfBundle))
                    path == "$api/skills/global/pdf-tools/file" -> {
                        if (query == "reference.md") {
                            val index = fixture.referenceReads.incrementAndGet()
                            val content =
                                if (fixture.referenceUpdated) "Updated reference"
                                else "Old reference"
                            (if (index == 1) fixture.oldReferenceGate else fixture.newReferenceGate)
                                ?.await(60, java.util.concurrent.TimeUnit.SECONDS)
                            if (index == 1) fixture.oldReferenceReturned = true
                            json(
                                wireJson.encodeToString(
                                    SkillFileContent(content, size = content.length.toLong())
                                )
                            )
                        } else
                            json(
                                """{"content":"---\nname: pdf-tools\ndescription: Extraire du texte et des tableaux depuis des PDF.\n---\n\n# PDF\n","encoding":"utf-8","size":120,"executable":false}"""
                            )
                    }
                    path == "$api/skills/global/pdf-tools/source" ->
                        json(wireJson.encodeToString(sourceStatus))
                    path == "$api/skills/global/pdf-tools/source/update" -> {
                        fixture.referenceUpdated = true
                        fixture.updates +=
                            wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                        json(
                            wireJson.encodeToString(
                                pdfBundle.copy(
                                    revision = "88bb02",
                                    source =
                                        pdfSource.copy(commit = "c0ffee1234567", modified = false),
                                )
                            )
                        )
                    }
                    path == "$api/skills/global/import" -> {
                        val body = wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
                        fixture.imports += body
                        if (body["path"] == null)
                            json(
                                """{"imported":null,"candidates":["skills/pdf","skills/docx","skills/xlsx"]}"""
                            )
                        else
                            json(
                                """{"imported":${wireJson.encodeToString(pdfBundle)},"candidates":[]}"""
                            )
                    }
                    path == "$api/overview" -> json("{}")
                    path.endsWith("/models") -> json("""{"models":[]}""")
                    else -> json("[]")
                }
            }
        }

    private val sourceStatus =
        SkillSourceStatus(
            pdfSource,
            SkillSourceCommit(
                "c0ffee1234567",
                "Extract tables with pdfplumber\n\nDetails",
                "2026-10-12T09:30:00Z",
            ),
            current = false,
            changes =
                listOf(
                    SkillSourceChange(
                        "scripts/extract.py",
                        "modified",
                        lines =
                            listOf(
                                SkillDiffLine("gap", "12 lignes inchangées"),
                                SkillDiffLine("context", "def extract(path):"),
                                SkillDiffLine("removed", "    return pdftotext(path)"),
                                SkillDiffLine("added", "    with pdfplumber.open(path) as pdf:"),
                                SkillDiffLine(
                                    "added",
                                    "        return [page.extract_tables() for page in pdf.pages]",
                                ),
                                SkillDiffLine("context", ""),
                                SkillDiffLine("gap", "40 lignes inchangées"),
                            ),
                    ),
                    SkillSourceChange(
                        "reference.md",
                        "added",
                        lines =
                            listOf(
                                SkillDiffLine("added", "# Tables"),
                                SkillDiffLine("added", "Use `extract_tables` for grids."),
                            ),
                    ),
                    SkillSourceChange("assets/sample.pdf", "removed", binary = true),
                ),
        )

    private var keyboard: androidx.compose.ui.platform.SoftwareKeyboardController? = null

    private fun launch(
        fixture: Fixture,
        role: String = "owner",
        dark: Boolean = false,
    ): MockWebServer {
        val server = MockWebServer()
        server.dispatcher = skillDispatcher(fixture, role)
        server.start()
        val vm =
            CairnViewModel(
                ApplicationProvider.getApplicationContext<Application>(),
                object : SessionVault {
                    private val values = mutableMapOf<String, String>()

                    override fun read(origin: String) = values[origin]

                    override fun write(origin: String, cookie: String?) {
                        if (cookie == null) values.remove(origin) else values[origin] = cookie
                    }
                },
                beaconOrigin = "",
            )
        compose.setContent {
            keyboard = androidx.compose.ui.platform.LocalSoftwareKeyboardController.current
            val state by vm.state.collectAsStateWithLifecycle()
            LaunchedEffect(Unit) {
                vm.state.first { it.ready }
                vm.connect(server.url("/").toString())
            }
            CairnTheme(if (dark) "dark" else "light") {
                // Like the app scaffold: background colour and matching content colour.
                androidx.compose.material3.Surface(
                    Modifier.fillMaxSize().systemBarsPadding().imePadding(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    if (state.session.authenticated && state.skills.isNotEmpty())
                        SkillsScreen(vm, state)
                }
            }
        }
        shown(hasTestTag("skill-card-global-review"), 20000)
        return server
    }

    private fun shown(matcher: SemanticsMatcher, timeout: Long = 15000) =
        compose.waitUntil(timeout) {
            compose.onAllNodes(matcher).fetchSemanticsNodes().isNotEmpty()
        }

    private fun gone(matcher: SemanticsMatcher) =
        compose.waitUntil(15000) { compose.onAllNodes(matcher).fetchSemanticsNodes().isEmpty() }

    /** Waits for the tree, then scrolls the row of [path] into view. */
    private fun treeRow(path: String): SemanticsNodeInteraction {
        shown(hasTestTag("skill-files"))
        val row = hasTestTag("skill-file-$path") or hasTestTag("skill-folder-$path")
        compose.waitUntil(15000) {
            runCatching { compose.onNodeWithTag("skill-files").performScrollToNode(row) }.isSuccess
        }
        return compose.onNode(row)
    }

    private fun hideKeyboard() {
        compose.runOnIdle { keyboard?.hide() }
        compose.waitForIdle()
    }

    private fun openFile(path: String) {
        treeRow(path).performClick()
        shown(hasTestTag("skill-editor"))
    }

    private fun backToFiles() {
        compose.onNodeWithContentDescription("Fichiers").performClick()
        shown(hasTestTag("skill-files"))
    }

    private fun fileActions(path: String) {
        treeRow(path)
        compose
            .onNodeWithContentDescription("Actions pour ${path.substringAfterLast('/')}")
            .performClick()
        shown(hasTestTag("skill-file-actions"))
    }

    @Test
    fun delayedDeletionDoesNotCloseTheNextSkillDraft() {
        val gate = java.util.concurrent.CountDownLatch(1)
        val fixture = Fixture().apply { deleteGate = gate }
        launch(fixture).use {
            try {
                compose.onNodeWithTag("skill-card-global-review").performClick()
                shown(hasTestTag("skill-workbench"))
                compose.onNodeWithContentDescription("Plus d’actions").performClick()
                compose.onNodeWithText("Supprimer le skill").performClick()
                compose.onNodeWithText("Confirmer").performClick()
                compose.waitUntil(15000) { fixture.deleteStarted }
                compose.onNodeWithContentDescription("Retour").performClick()
                shown(hasTestTag("skill-card-global-pdf-tools"))
                compose.onNodeWithTag("skill-card-global-pdf-tools").performClick()
                openFile("SKILL.md")
                shown(hasText("# PDF", substring = true))
                val edited =
                    "---\nname: pdf-tools\ndescription: Draft B\n---\nB survives deletion\n"
                compose.onNodeWithTag("skill-code-editor").performTextReplacement(edited)
                gate.countDown()
                compose.waitUntil(15000) { fixture.deleteReturned }
                compose.waitForIdle()
                Thread.sleep(500)
                compose.waitForIdle()
                compose
                    .onNodeWithTag("skill-code-editor")
                    .assertTextContains("B survives deletion", substring = true)
            } finally {
                gate.countDown()
            }
        }
    }

    @Test
    fun replacingADeletedFolderWithAFileKeepsBothTreeRowsUsable() {
        launch(Fixture()).use {
            compose.onNodeWithTag("skill-card-global-review").performClick()
            shown(hasTestTag("skill-files"))
            fileActions("scripts")
            compose.onNodeWithText("Supprimer le dossier").performClick()
            gone(hasTestTag("skill-file-actions"))
            compose.onNodeWithContentDescription("Nouveau fichier").performClick()
            compose.onNodeWithTag("skill-path-field").performTextReplacement("scripts")
            compose.onNodeWithTag("skill-path-confirm").performClick()
            shown(hasTestTag("skill-editor"))
            backToFiles()
            compose
                .onNodeWithTag("skill-files")
                .performScrollToNode(hasTestTag("skill-file-scripts"))
            compose.onNodeWithTag("skill-file-scripts").assertExists()
            compose.onNodeWithTag("skill-folder-scripts").assertExists()
            compose.onNodeWithTag("skill-save").assertIsEnabled()
        }
    }

    @Test
    fun savingMakesFileMutationsUnavailableUntilTheResponse() {
        val gate = java.util.concurrent.CountDownLatch(1)
        val fixture = Fixture().apply { saveGate = gate }
        launch(fixture).use {
            try {
                compose.onNodeWithTag("skill-card-global-review").performClick()
                openFile("SKILL.md")
                shown(hasText(skillMarkdown.trimEnd(), substring = true))
                compose
                    .onNodeWithTag("skill-code-editor")
                    .performTextReplacement(skillMarkdown + "More instructions\n")
                backToFiles()
                compose.onNodeWithTag("skill-save").performClick()
                compose.waitUntil(15000) { fixture.batches.size == 1 }
                compose.onNodeWithTag("skill-save").assertIsNotEnabled()
                compose.onNodeWithContentDescription("Nouveau fichier").assertDoesNotExist()
                compose
                    .onNodeWithContentDescription("Ajouter depuis l’appareil")
                    .assertDoesNotExist()
                treeRow("old.md")
                compose.onNodeWithContentDescription("Actions pour old.md").assertDoesNotExist()
            } finally {
                gate.countDown()
            }
            shown(hasContentDescription("Nouveau fichier"))
        }
    }

    @Test
    fun delayedSaveDoesNotReopenAnAbandonedSkillOrOverwriteTheNextDraft() {
        val gate = java.util.concurrent.CountDownLatch(1)
        val fixture = Fixture().apply { saveGate = gate }
        launch(fixture).use {
            try {
                compose.onNodeWithTag("skill-card-global-review").performClick()
                openFile("SKILL.md")
                shown(hasText(skillMarkdown.trimEnd(), substring = true))
                compose
                    .onNodeWithTag("skill-code-editor")
                    .performTextReplacement(skillMarkdown + "A draft\n")
                compose.onNodeWithTag("skill-save").performClick()
                compose.waitUntil(15000) { fixture.batches.size == 1 }
                hideKeyboard()
                compose.onNodeWithContentDescription("Retour").performClick()
                shown(hasText("Abandonner les modifications ?"))
                compose.onNodeWithText("Abandonner", substring = false).performClick()
                shown(hasTestTag("skill-card-global-pdf-tools"))
                compose.onNodeWithTag("skill-card-global-pdf-tools").performClick()
                openFile("SKILL.md")
                shown(hasText("# PDF", substring = true))
                val edited = "---\nname: pdf-tools\ndescription: Draft B\n---\nB stays here\n"
                compose.onNodeWithTag("skill-code-editor").performTextReplacement(edited)
                gate.countDown()
                compose.waitUntil(15000) { fixture.saveReturned }
                // Drain the released HTTP response and subsequent main-thread UI work.
                compose.waitForIdle()
                Thread.sleep(500)
                compose.waitForIdle()
                compose
                    .onNodeWithTag("skill-code-editor")
                    .assertTextContains("B stays here", substring = true)
                compose.onNodeWithText("pdf-tools", substring = false).assertExists()
            } finally {
                gate.countDown()
            }
        }
    }

    @Test
    fun membersCanReadSourceMetadataWithoutOwnerOnlyActions() {
        launch(Fixture(), role = "member").use {
            compose.onNodeWithTag("skill-card-global-pdf-tools").performClick()
            shown(hasTestTag("skill-source-chip"))
            compose.onNodeWithTag("skill-source-chip").performClick()
            shown(hasTestTag("skill-source-sheet"))
            shown(hasText("anthropics/skills", substring = false))
            compose.onNodeWithTag("skill-source-check").assertDoesNotExist()
            compose.onNodeWithTag("skill-source-update").assertDoesNotExist()
        }
    }

    @Test
    fun sourceUpdatesIgnoreFileReadsFromThePrecedingRevision() {
        val oldGate = java.util.concurrent.CountDownLatch(1)
        val newGate = java.util.concurrent.CountDownLatch(1)
        val fixture =
            Fixture().apply {
                oldReferenceGate = oldGate
                newReferenceGate = newGate
            }
        launch(fixture).use {
            try {
                compose.onNodeWithTag("skill-card-global-pdf-tools").performClick()
                openFile("reference.md")
                compose.waitUntil(15000) { fixture.referenceReads.get() == 1 }
                backToFiles()
                compose.onNodeWithTag("skill-source-chip").performClick()
                shown(hasTestTag("skill-source-check"))
                compose.onNodeWithTag("skill-source-check").performClick()
                shown(hasTestTag("skill-source-update"))
                compose.onNodeWithTag("skill-source-update").performScrollTo().performClick()
                shown(hasTestTag("skill-source-update-confirm"))
                compose.onNodeWithTag("skill-source-update-confirm").performClick()
                gone(hasTestTag("skill-source-sheet"))
                openFile("reference.md")
                compose.waitUntil(15000) { fixture.referenceReads.get() == 2 }
                oldGate.countDown()
                compose.waitUntil(15000) { fixture.oldReferenceReturned }
                compose.waitForIdle()
                Thread.sleep(500)
                newGate.countDown()
                shown(hasText("Updated reference", substring = true))
                compose
                    .onNodeWithTag("skill-code-editor")
                    .assertTextContains("Updated reference", substring = true)
            } finally {
                oldGate.countDown()
                newGate.countDown()
            }
        }
    }

    @Test
    fun ownerEditsSeveralFilesAndSavesThemInOneBatch() {
        val fixture = Fixture()
        launch(fixture).use {
            compose.onNodeWithText("6 fichiers").assertExists()
            compose.onNodeWithText("anthropics/skills@9b1e2c4").assertExists()
            capture("skills-list")

            compose.onNodeWithTag("skill-card-global-review").performClick()
            treeRow("scripts/check.sh")
            shown(
                hasText(
                    "Relire une branche, lancer les vérifications et rédiger un compte rendu clair."
                )
            )
            listOf("SKILL.md", "assets", "references", "scripts", "old.md").forEach {
                treeRow(it).assertExists()
            }
            treeRow("SKILL.md")
            compose.onNodeWithTag("skill-save").assertIsNotEnabled()
            capture("workbench-tree")

            openFile("scripts/check.sh")
            shown(hasText(checkScript.trimEnd(), substring = true))
            compose.onNodeWithTag("skill-executable").assertIsSelected()
            compose
                .onNodeWithTag("skill-code-editor")
                .performTextReplacement(checkScript + "cargo clippy --quiet\n")
            shown(hasText("modifié"))
            hideKeyboard()
            capture("workbench-script")
            backToFiles()

            openFile("scripts/lint.py")
            shown(hasText("import subprocess", substring = true))
            compose.onNodeWithTag("skill-executable").assertIsNotSelected().performClick()
            compose.onNodeWithTag("skill-executable").assertIsSelected()
            backToFiles()

            openFile("assets/logo.png")
            shown(hasTestTag("skill-image"))
            shown(hasText("192 × 128 px", substring = true))
            capture("workbench-image")
            backToFiles()

            compose.onNodeWithContentDescription("Nouveau fichier").performClick()
            compose.onNodeWithTag("skill-path-field").performTextReplacement("../escape.md")
            compose.onNodeWithTag("skill-path-confirm").assertIsNotEnabled()
            compose
                .onNodeWithTag("skill-path-field")
                .performTextReplacement("references/checklist.md")
            compose.onNodeWithTag("skill-path-confirm").performClick()
            shown(hasTestTag("skill-editor"))
            compose.onNodeWithTag("skill-code-editor").performTextReplacement("- [ ] Tests\n")
            backToFiles()

            fileActions("old.md")
            compose.onNodeWithText("Supprimer").performClick()
            gone(hasTestTag("skill-file-actions"))
            shown(hasText("supprimé"))

            fileActions("references/guide.md")
            compose.onNodeWithText("Renommer ou déplacer").performClick()
            compose.onNodeWithTag("skill-path-field").performTextReplacement("docs/guide.md")
            compose.onNodeWithTag("skill-path-confirm").performClick()
            treeRow("docs/guide.md")
            shown(hasText("déplacé"))
            treeRow("SKILL.md")
            shown(hasText("5 modifications non enregistrées"))
            capture("workbench-changes")

            compose.onNodeWithTag("skill-changes").performClick()
            shown(hasText("references/guide.md → docs/guide.md"))
            capture("workbench-changes-sheet")
            compose.onNodeWithText("Suppression de old.md").assertExists()
            compose.onNodeWithContentDescription("Fermer les modifications").performClick()
            gone(hasTestTag("skill-changes-sheet"))

            compose.onNodeWithTag("skill-save").assertIsEnabled().performClick()
            compose.waitUntil(15000) { fixture.batches.size == 1 }
            val (route, batch) = fixture.batches.single()
            assertEquals("$api/skills/global/review/bundle", route)
            assertEquals("4f1c0d2e9a7b", batch["base"]!!.jsonPrimitive.content)
            val changes = batch["changes"]!!.jsonArray.map { it.jsonObject }
            assertEquals(
                listOf("delete", "move", "write", "write", "mode"),
                changes.map { it["op"]!!.jsonPrimitive.content },
            )
            assertEquals("old.md", changes[0]["path"]!!.jsonPrimitive.content)
            assertEquals("references/guide.md", changes[1]["from"]!!.jsonPrimitive.content)
            assertEquals("docs/guide.md", changes[1]["to"]!!.jsonPrimitive.content)
            assertEquals("scripts/check.sh", changes[2]["path"]!!.jsonPrimitive.content)
            assertEquals(
                checkScript + "cargo clippy --quiet\n",
                changes[2]["content"]!!.jsonPrimitive.content,
            )
            assertEquals("true", changes[2]["executable"]!!.jsonPrimitive.content)
            assertEquals("references/checklist.md", changes[3]["path"]!!.jsonPrimitive.content)
            assertEquals("- [ ] Tests\n", changes[3]["content"]!!.jsonPrimitive.content)
            assertEquals("scripts/lint.py", changes[4]["path"]!!.jsonPrimitive.content)
            assertEquals("true", changes[4]["executable"]!!.jsonPrimitive.content)

            gone(hasTestTag("skill-changes"))
            gone(hasText("déplacé"))
            compose.onNodeWithTag("skill-file-old.md").assertDoesNotExist()
            compose.onNodeWithTag("skill-save").assertIsNotEnabled()
        }
    }

    @Test
    fun conflictingSaveExplainsAndReloadsTheLatestVersion() {
        val fixture = Fixture().apply { conflict = true }
        launch(fixture).use {
            compose.onNodeWithTag("skill-card-global-review").performClick()
            openFile("SKILL.md")
            shown(hasText("# Revue de code", substring = true))
            compose
                .onNodeWithTag("skill-code-editor")
                .performTextReplacement(skillMarkdown + "\n4. Vérifier la documentation.\n")
            compose.onNodeWithTag("skill-mode-preview").performClick()
            shown(hasTestTag("skill-preview"))
            compose.onNodeWithTag("skill-save").performClick()
            shown(hasTestTag("skill-conflict"))
            compose
                .onNodeWithText("Ce skill a été modifié ailleurs depuis son ouverture.")
                .assertExists()
            capture("workbench-conflict")
            val loads = fixture.bundleLoads
            compose.onNodeWithText("Recharger").performClick()
            compose.waitUntil(15000) { fixture.bundleLoads == loads + 1 }
            gone(hasTestTag("skill-conflict"))
            compose.onNodeWithTag("skill-save").assertIsNotEnabled()
        }
    }

    @Test
    fun leavingWithUnsavedChangesAsksBeforeDiscarding() {
        launch(Fixture()).use {
            compose.onNodeWithTag("skill-card-global-review").performClick()
            treeRow("old.md")
            fileActions("old.md")
            compose.onNodeWithText("Supprimer").performClick()
            gone(hasTestTag("skill-file-actions"))
            compose.onNodeWithContentDescription("Retour").performClick()
            shown(hasText("Abandonner les modifications ?"))
            compose.onNodeWithText("Continuer l’édition").performClick()
            compose.onNodeWithTag("skill-workbench").assertExists()
            compose.onNodeWithContentDescription("Restaurer old.md").performClick()
            compose.onNodeWithContentDescription("Retour").performClick()
            shown(hasTestTag("skill-card-global-review"))
        }
    }

    @Test fun githubSourceShowsTheDiffAndUpdatesFromTheReviewedCommit() = sourceJourney(false)

    @Test fun darkSourceDiffKeepsReadableColours() = sourceJourney(true)

    private fun sourceJourney(dark: Boolean) {
        val fixture = Fixture()
        launch(fixture, dark = dark).use {
            compose.onNodeWithTag("skill-card-global-pdf-tools").performClick()
            shown(hasTestTag("skill-source-chip"))
            compose.onNodeWithTag("skill-source-chip").performClick()
            shown(hasTestTag("skill-source-modified"))
            compose.onNodeWithTag("skill-source-check").performClick()
            shown(hasTestTag("skill-source-change-scripts/extract.py"))
            compose.onNodeWithText("Nouvelle version disponible").assertExists()
            compose.onNodeWithText("12 lignes inchangées").assertExists()
            compose.onNodeWithText("    with pdfplumber.open(path) as pdf:").assertExists()
            compose.onNodeWithText("Fichier binaire : le contenu n’est pas comparé.").assertExists()
            capture(if (dark) "source-diff-dark" else "source-diff")
            if (dark) return
            compose.onNodeWithTag("skill-source-update").performScrollTo().performClick()
            shown(hasText("Mettre à jour depuis GitHub ?"))
            compose.onNodeWithTag("skill-source-update-confirm").performClick()
            compose.waitUntil(15000) { fixture.updates.size == 1 }
            assertEquals("c0ffee1234567", fixture.updates[0]["commit"]!!.jsonPrimitive.content)
            assertEquals("77aa01", fixture.updates[0]["base"]!!.jsonPrimitive.content)
            gone(hasTestTag("skill-source-sheet"))
            shown(hasText("anthropics/skills@c0ffee1"))
        }
    }

    @Test
    fun githubImportLetsTheOwnerChooseAmongSeveralSkills() {
        val fixture = Fixture()
        launch(fixture).use {
            compose.onNodeWithTag("skill-import").performClick()
            shown(hasTestTag("skill-import-sheet"))
            compose.onNodeWithTag("skill-import-github").performClick()
            compose.onNodeWithTag("skill-import-repository").performTextInput("anthropics/skills")
            compose.onNodeWithTag("skill-import-ref").performTextInput("main")
            compose.onNodeWithTag("skill-import-submit").performScrollTo().performClick()
            shown(hasTestTag("skill-candidate-skills/pdf"))
            assertNull(fixture.imports[0]["path"])
            assertEquals("github", fixture.imports[0]["source"]!!.jsonPrimitive.content)
            compose.onNodeWithTag("skill-import-submit").assertIsNotEnabled()
            compose.onNodeWithTag("skill-candidate-skills/pdf").performScrollTo().performClick()
            hideKeyboard()
            capture("import-candidates")
            compose.onNodeWithTag("skill-import-submit").performScrollTo().performClick()
            compose.waitUntil(15000) { fixture.imports.size == 2 }
            assertEquals("skills/pdf", fixture.imports[1]["path"]!!.jsonPrimitive.content)
            assertEquals("main", fixture.imports[1]["ref"]!!.jsonPrimitive.content)
            assertEquals(
                "anthropics/skills",
                fixture.imports[1]["repository"]!!.jsonPrimitive.content,
            )
            shown(hasTestTag("skill-workbench"))
            treeRow("scripts/extract.py")
        }
    }

    @Test
    fun ownerCreatesANewSkillWithANullBase() {
        val fixture = Fixture()
        launch(fixture).use {
            compose.onNodeWithTag("skill-new").performClick()
            compose.onNodeWithTag("skill-new-name").performTextInput("Release")
            compose.onNodeWithTag("skill-new-confirm").assertIsNotEnabled()
            compose.onNodeWithTag("skill-new-name").performTextReplacement("review")
            compose.onNodeWithTag("skill-new-confirm").assertIsNotEnabled()
            compose.onNodeWithTag("skill-new-name").performTextReplacement("release-checklist")
            compose.onNodeWithTag("skill-new-confirm").performClick()
            treeRow("SKILL.md")
            compose.onNodeWithTag("skill-save").assertIsEnabled().performClick()
            compose.waitUntil(15000) { fixture.batches.size == 1 }
            val (route, batch) = fixture.batches.single()
            assertEquals("$api/skills/global/release-checklist/bundle", route)
            assertEquals(JsonNull, batch["base"])
            val write = batch["changes"]!!.jsonArray.single().jsonObject
            assertEquals("SKILL.md", write["path"]!!.jsonPrimitive.content)
            assertTrue(
                write["content"]!!
                    .jsonPrimitive
                    .content
                    .startsWith("---\nname: release-checklist\n")
            )
            compose.waitUntil(15000) {
                compose.onAllNodesWithText("Global").fetchSemanticsNodes().isNotEmpty() &&
                    compose
                        .onAllNodesWithText("Nouveau skill", substring = true)
                        .fetchSemanticsNodes()
                        .isEmpty()
            }
        }
    }

    @Test
    fun membersBrowseSkillsReadOnly() {
        launch(Fixture(), role = "member").use {
            compose.onNodeWithTag("skill-new").assertDoesNotExist()
            compose.onNodeWithTag("skill-import").assertDoesNotExist()
            compose.onNodeWithTag("skill-card-global-review").performClick()
            treeRow("scripts/check.sh")
            compose.onNodeWithTag("skill-save").assertDoesNotExist()
            compose.onNodeWithContentDescription("Nouveau fichier").assertDoesNotExist()
            compose.onNodeWithContentDescription("Actions pour old.md").assertDoesNotExist()
            openFile("SKILL.md")
            shown(hasTestTag("skill-preview"))
            backToFiles()
            openFile("scripts/check.sh")
            shown(hasText(checkScript.trimEnd(), substring = true))
            compose.onNodeWithTag("skill-executable").assertIsNotEnabled()
            // Read-only: no edit reaches the draft, even through accessibility actions.
            runCatching {
                compose.onNodeWithTag("skill-code-editor").performTextReplacement("rm -rf /\n")
            }
            compose.waitForIdle()
            compose.onNodeWithText("modifié").assertDoesNotExist()
        }
    }

    @Test
    fun darkWorkbenchRendersTreeAndScript() {
        launch(Fixture(), dark = true).use {
            capture("skills-list-dark")
            compose.onNodeWithTag("skill-card-global-review").performClick()
            treeRow("scripts/check.sh")
            capture("workbench-tree-dark")
            openFile("scripts/check.sh")
            shown(hasText(checkScript.trimEnd(), substring = true))
            capture("workbench-script-dark")
        }
    }
}

/** A 192 × 128 PNG: a small cairn at dusk. */
private const val CAIRN_PNG =
    "iVBORw0KGgoAAAANSUhEUgAAAMAAAACACAIAAADS5vE8AAADKElEQVR42u3bIYtUYRjF8ecTGEQEQQRBBEEUQRa0GCwGi6Jlgy4IYlgQEYOWNZlMNpPJZvKjGASjxWQSu65MHNmdvTPve99zzh/+cWAu5/lxYcLUia1toskVExCACEAEIAIQEYAIQCQD6OT1HaLJAYgARAAiABGAiABEACIdQKduPCbp/nx5NeO3A0hSzMEBiCbS6c8IQJ56uhkCkK2ePoYA5KyngyEAmetpbQhA/nqaGqrTN3dp2DYIqNETAihCTztDAAIQgAAEIAIQAYh89TQyBCDeQOsBOnPrOY3ZxgG1eEgAAQhALft27zKAALQSlKMGIABNcdNT0uB6ogFthE5rRgBK0TO4oXZLFnrsDTUds87efhlVUz2LWjz2ZD2t9wSQBqBphjrsWehxNdRnUt5AMnpWZ9RzTwDpAVr2NOOecYBaG0obMxFQO0OBS4YCamEoc8Y6d2cvuY3QSR4wHdA6khgNQHu/f/08uH0oh34GQIjZZADCDZIANCudHEYFHRgBaHQ9xobq/P03fg2oZ5Hf1IUeDAEIQADK0ONniDcQegAEIADxKwxAGIrT8w/Qhe23rg2lx3VkZ0CDMPKe1x/QjIwShk0B1FNS1J5xgFp4Sh4wFNCP719XbN/H6h8GEGI2GYBAA6bDAF18+M6sodwsZ7Z24QZJ6YDk6DgxKvRgKBeQgR51Q4UeDCUCMtOjawhAAFoP0KVH7+Wy1LNI7haSgFwNKR4CQACKBORnSPQKwoCcDOmeQBuQhyHp/eUBqRtSH98BkCgjj9nrypMPZg3uxmxtQ0BjSnId2RnQ7JgShk0B1MdT4JJZgCb852vxz56jBqBEK60DEGLw5AVIDo0fprq6+1EuGzfLyd2ioAOjCEAhdOQYFXRg5AwoXM/4hgo9GPIEBBoJQwAC0HqAtp59GjC4/LcBLzUoIAxJ6AEQgHwBYWh8PaMDwtDgegQAYWjw6wgAimUkcRcZQFGMhC5S1158lsuYjtwt6vjrp7rZuNE9gTYgaUwes/sAGt+T5c517MHdnLpZyZk0C9A0c4wAIAIQAYgARAQgAhABiABEAGICAhDN1l/rcaUbETpSiwAAAABJRU5ErkJggg=="
