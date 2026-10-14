package build.cairn.app.data

/*
 * Local editing of a skill bundle. Every edit stays in a [SkillDraft] until the owner saves;
 * [SkillDraft.changes] then produces the ordered batch for `POST …/bundle`.
 */

const val SKILL_FILE = "SKILL.md"
const val SKILL_MARKDOWN_BYTES = 100_000L

/** A file of the edited skill. [origin] is its path in the base revision, null when new. */
data class DraftFile(
    val path: String,
    val origin: String?,
    val size: Long,
    val executable: Boolean,
    val text: Boolean,
    /** Content written in this draft; null while the file keeps its saved content. */
    val body: SkillBody? = null,
)

enum class DraftStatus {
    UNCHANGED,
    NEW,
    MODIFIED,
    MOVED,
    DELETED,
}

data class SkillDraft(
    val name: String,
    val base: SkillBundle?,
    val files: List<DraftFile>,
    /** Folders created by the user; folders holding files are implicit. */
    val folders: Set<String> = emptySet(),
    /**
     * Saved contents already fetched, by base path, to detect edits that return to the original.
     */
    val originals: Map<String, SkillBody> = emptyMap(),
) {
    val limits: SkillLimits
        get() = base?.limits ?: SkillLimits()

    private val baseFiles: Map<String, SkillFile>
        get() = base?.files.orEmpty().associateBy { it.path }

    fun file(path: String): DraftFile? = files.find { it.path == path }

    fun original(file: DraftFile): SkillFile? = file.origin?.let { baseFiles[it] }

    /** Base files removed in this draft, still listed so they can be restored. */
    val deleted: List<SkillFile>
        get() {
            val kept = files.mapNotNull { it.origin }.toSet()
            return base?.files.orEmpty().filter { it.path !in kept }
        }

    fun status(file: DraftFile): DraftStatus {
        val original = original(file) ?: return DraftStatus.NEW
        return when {
            file.body != null || file.executable != original.executable -> DraftStatus.MODIFIED
            file.path != original.path -> DraftStatus.MOVED
            else -> DraftStatus.UNCHANGED
        }
    }

    val totalBytes: Long
        get() = files.sumOf { it.size }

    val executables: Int
        get() = files.count { it.executable }

    val dirty: Boolean
        get() = base == null || changes().isNotEmpty()

    /** Current content when it is known locally. */
    fun content(path: String): SkillBody? {
        val file = file(path) ?: return null
        return file.body ?: file.origin?.let { originals[it] }
    }

    fun withOriginal(origin: String, body: SkillBody) =
        copy(originals = originals + (origin to body))

    /** Replaces the text of [path]; returning to the saved text clears the edit. */
    fun edit(path: String, text: String): SkillDraft {
        val file = requireNotNull(file(path)) { "Fichier introuvable." }
        val body = SkillBody.Text(text)
        val unchanged = file.origin != null && originals[file.origin] == body
        return replace(
            path,
            file.copy(body = if (unchanged) null else body, size = body.size, text = true),
        )
    }

    /** Adds a file, or replaces the content of an existing one (from the device for example). */
    fun put(path: String, body: SkillBody, executable: Boolean? = null): SkillDraft {
        skillPathProblem(path)?.let { throw IllegalArgumentException(it) }
        val existing = file(path)
        val text = body is SkillBody.Text
        if (existing != null)
            return replace(
                path,
                existing.copy(
                    body = body,
                    size = body.size,
                    text = text,
                    executable = executable ?: existing.executable,
                ),
            )
        require(path !in folderPaths()) { "Un dossier porte déjà ce nom." }
        val shebang = body is SkillBody.Text && body.value.startsWith("#!")
        return copy(
            files =
                files + DraftFile(path, null, body.size, executable ?: shebang, text, body = body),
            folders = folders - path,
        )
    }

    fun addFolder(path: String): SkillDraft {
        val clean = path.trim('/')
        skillPathProblem(clean)?.let { throw IllegalArgumentException(it) }
        require(file(clean) == null) { "Un fichier porte déjà ce nom." }
        return copy(folders = folders + clean)
    }

    fun move(from: String, to: String): SkillDraft {
        if (from == to) return this
        val file = requireNotNull(file(from)) { "Fichier introuvable." }
        require(from != SKILL_FILE) { "SKILL.md reste à la racine du skill." }
        skillPathProblem(to)?.let { throw IllegalArgumentException(it) }
        require(file(to) == null && to !in folderPaths()) { "Ce chemin est déjà utilisé." }
        return replace(from, file.copy(path = to))
    }

    fun moveFolder(from: String, to: String): SkillDraft {
        val source = from.trim('/')
        val target = to.trim('/')
        if (source == target) return this
        skillPathProblem(target)?.let { throw IllegalArgumentException(it) }
        require(!target.startsWith("$source/")) {
            "Un dossier ne peut pas être déplacé dans lui-même."
        }
        val moved = files.filter { it.path.startsWith("$source/") }
        val renamed = moved.associate { it.path to target + it.path.removePrefix(source) }
        renamed.values.forEach { skillPathProblem(it)?.let { problem -> error(problem) } }
        val others = files.filter { it.path !in renamed }.map { it.path }.toSet()
        require(renamed.values.none { it in others } && target !in others) {
            "Ce chemin est déjà utilisé."
        }
        return copy(
            files = files.map { renamed[it.path]?.let { path -> it.copy(path = path) } ?: it },
            folders =
                folders
                    .map {
                        if (it == source || it.startsWith("$source/"))
                            target + it.removePrefix(source)
                        else it
                    }
                    .toSet(),
        )
    }

    fun delete(path: String): SkillDraft {
        require(path != SKILL_FILE) { "SKILL.md est obligatoire." }
        return copy(files = files.filter { it.path != path })
    }

    fun deleteFolder(path: String): SkillDraft {
        val prefix = path.trim('/') + "/"
        return copy(
            files = files.filter { !it.path.startsWith(prefix) },
            folders = folders.filter { it != prefix.dropLast(1) && !it.startsWith(prefix) }.toSet(),
        )
    }

    /** Brings back a deleted base file at its saved path. */
    fun restore(origin: String): SkillDraft {
        val original = requireNotNull(baseFiles[origin]) { "Fichier introuvable." }
        require(file(origin) == null) { "Ce chemin est déjà utilisé." }
        return copy(
            files =
                files +
                    DraftFile(
                        original.path,
                        original.path,
                        original.size,
                        original.executable,
                        original.text,
                    )
        )
    }

    fun setExecutable(path: String, executable: Boolean): SkillDraft {
        val file = requireNotNull(file(path)) { "Fichier introuvable." }
        return replace(path, file.copy(executable = executable))
    }

    private fun replace(path: String, file: DraftFile) =
        copy(files = files.map { if (it.path == path) file else it })

    /** Folders shown in the tree: explicit ones and the parents of every file. */
    fun folderPaths(): Set<String> =
        (folders + files.flatMap { parents(it.path) } + deleted.flatMap { parents(it.path) })
            .flatMap { listOf(it) + parents(it) }
            .toSet()

    /**
     * The ordered batch: deletions free paths, then moves (cycles go through a temporary path),
     * then content writes on the final paths, then mode changes of untouched files.
     */
    fun changes(): List<SkillChange> {
        val result = mutableListOf<SkillChange>()
        deleted.forEach { result += SkillChange.Delete(it.path) }
        val pending =
            files
                .filter { it.origin != null && it.origin != it.path }
                .map { SkillChange.Move(it.origin!!, it.path) }
                .toMutableList()
        val occupied = (files.map { it.path } + files.mapNotNull { it.origin }).toMutableSet()
        var temporary = 0
        while (pending.isNotEmpty()) {
            // A move waits while its target, one of the target's parent folders, or a folder at
            // the target path is still held by a file that moves later in the batch.
            val ready = pending.firstOrNull { move ->
                pending.none { it !== move && pathsCollide(move.to, it.from) }
            }
            if (ready != null) {
                result += ready
                pending.remove(ready)
                continue
            }
            // Every remaining move waits for another one: park one file to break the cycle.
            val parked = pending.removeAt(0)
            var path: String
            do {
                path = "cairn-move-${temporary++}-${parked.from.substringAfterLast('/')}"
            } while (path in occupied)
            occupied += path
            result += SkillChange.Move(parked.from, path)
            pending += SkillChange.Move(path, parked.to)
        }
        files
            .filter { it.body != null }
            .forEach { result += SkillChange.Write(it.path, it.body!!, it.executable) }
        files
            .filter {
                it.body == null && original(it)?.executable?.let { e -> e != it.executable } == true
            }
            .forEach { result += SkillChange.Mode(it.path, it.executable) }
        return result
    }

    /** Problems that prevent saving, in French, using the server limits. */
    fun problems(): List<String> {
        val limits = limits
        val problems = mutableListOf<String>()
        val skill = file(SKILL_FILE)
        if (skill == null) problems += "Le skill doit contenir SKILL.md."
        else {
            if (!skill.text) problems += "SKILL.md doit être un texte UTF-8."
            if (skill.size > SKILL_MARKDOWN_BYTES)
                problems += "SKILL.md dépasse ${formatBytes(SKILL_MARKDOWN_BYTES)}."
            (content(SKILL_FILE) as? SkillBody.Text)?.let { text ->
                val declared = skillFrontmatter(text.value)["name"]
                if (declared != null && declared != name)
                    problems += "Le nom déclaré dans SKILL.md doit être « $name »."
                if (declared == null) problems += "SKILL.md doit déclarer « name: $name »."
            }
        }
        files
            .filter { it.size > limits.fileBytes }
            .forEach { problems += "${it.path} dépasse ${formatBytes(limits.fileBytes)}." }
        if (files.size > limits.files)
            problems += "Un skill compte au plus ${limits.files} fichiers."
        if (totalBytes > limits.bundleBytes)
            problems += "Le skill dépasse ${formatBytes(limits.bundleBytes)} au total."
        return problems
    }

    companion object {
        fun of(bundle: SkillBundle) =
            SkillDraft(
                bundle.name,
                bundle,
                bundle.files.map { DraftFile(it.path, it.path, it.size, it.executable, it.text) },
            )

        fun create(name: String, description: String = "Décrire quand utiliser ce skill.") =
            SkillDraft(name, null, emptyList())
                .put(
                    SKILL_FILE,
                    SkillBody.Text(
                        "---\nname: $name\ndescription: $description\n---\n\n# $name\n\nDécrire la méthode, les fichiers de référence et les critères de réussite.\n"
                    ),
                )
    }
}

/** True when [a] and [b] are the same path, or one is a folder containing the other. */
fun pathsCollide(a: String, b: String): Boolean =
    a == b || a.startsWith("$b/") || b.startsWith("$a/")

fun parents(path: String): List<String> {
    val segments = path.split('/')
    return (1 until segments.size).map { segments.take(it).joinToString("/") }
}

val skillNamePattern = Regex("[a-z0-9][a-z0-9-]*")

/** Mirrors the server rules for a relative path inside a skill; null when valid. */
fun skillPathProblem(path: String): String? {
    if (path.isBlank()) return "Indiquez un chemin."
    if (path.startsWith("/") || path.endsWith("/")) return "Le chemin est relatif, sans / final."
    if (path.contains('\\')) return "Utilisez / pour séparer les dossiers."
    if (path.toByteArray(Charsets.UTF_8).size > 240) return "Le chemin dépasse 240 octets."
    val segments = path.split('/')
    if (segments.size > 8) return "Le chemin compte au plus 8 niveaux."
    if (segments.any { it.isBlank() }) return "Le chemin contient un dossier vide."
    if (segments.any { it.startsWith(".") })
        return "Les noms commençant par « . » ne font pas partie du skill."
    if (segments.any { segment -> segment.any { it.isISOControl() } })
        return "Le chemin contient des caractères invisibles."
    return null
}

/** `name`, `description`… from the YAML frontmatter, with surrounding quotes removed. */
fun skillFrontmatter(markdown: String): Map<String, String> {
    val text = markdown.replace("\r\n", "\n")
    if (!text.startsWith("---\n")) return emptyMap()
    val end = text.indexOf("\n---", 4)
    if (end < 0) return emptyMap()
    return text
        .substring(4, end)
        .lines()
        .mapNotNull { line ->
            val key = line.substringBefore(':', "").trim()
            if (key.isEmpty() || line.startsWith(" ")) null
            else
                key to
                    line.substringAfter(':').trim().removeSurrounding("\"").removeSurrounding("'")
        }
        .toMap()
}

/** The Markdown after the YAML frontmatter, for previews. */
fun skillMarkdownBody(markdown: String): String {
    val text = markdown.replace("\r\n", "\n")
    if (!text.startsWith("---\n")) return markdown
    val end = text.indexOf("\n---", 4)
    if (end < 0) return markdown
    return text.substring(end + 4).substringAfter('\n', "").trimStart('\n')
}

enum class SkillFileKind {
    SKILL,
    MARKDOWN,
    SCRIPT,
    IMAGE,
    TEXT,
    BINARY,
}

private val imageExtensions = setOf("png", "jpg", "jpeg", "gif", "webp", "bmp")
private val scriptExtensions =
    setOf("sh", "bash", "zsh", "fish", "py", "js", "mjs", "cjs", "ts", "rb", "pl", "php", "ps1")

fun skillFileKind(path: String, text: Boolean, executable: Boolean): SkillFileKind {
    val extension = path.substringAfterLast('/').substringAfterLast('.', "").lowercase()
    return when {
        path == SKILL_FILE -> SkillFileKind.SKILL
        extension in imageExtensions -> SkillFileKind.IMAGE
        !text -> SkillFileKind.BINARY
        extension in setOf("md", "markdown", "mdx") -> SkillFileKind.MARKDOWN
        executable || extension in scriptExtensions -> SkillFileKind.SCRIPT
        else -> SkillFileKind.TEXT
    }
}

/** One visible row of the file tree. */
data class SkillTreeRow(
    val path: String,
    val name: String,
    val depth: Int,
    val folder: Boolean,
    val deleted: Boolean = false,
)

/**
 * Flattens files and folders into tree rows: SKILL.md first, then folders and files sorted by name
 * at each level. Children of [collapsed] folders are hidden.
 */
fun skillTree(
    files: List<String>,
    folders: Set<String> = emptySet(),
    deleted: List<String> = emptyList(),
    collapsed: Set<String> = emptySet(),
): List<SkillTreeRow> {
    val allFolders =
        (folders + (files + deleted).flatMap { parents(it) })
            .flatMap { listOf(it) + parents(it) }
            .toSet()
    val entries =
        allFolders.map { Triple(it, true, false) } +
            files.map { Triple(it, false, false) } +
            deleted.filter { it !in files }.map { Triple(it, false, true) }
    val byParent = entries.groupBy { (path) -> path.substringBeforeLast('/', "") }
    val rows = mutableListOf<SkillTreeRow>()
    fun visit(parent: String, depth: Int) {
        val children =
            byParent[parent]
                .orEmpty()
                .sortedWith(
                    compareBy<Triple<String, Boolean, Boolean>>(
                            { it.first != SKILL_FILE },
                            { !it.second },
                        )
                        .thenBy(String.CASE_INSENSITIVE_ORDER) { it.first.substringAfterLast('/') }
                )
        for ((path, folder, removed) in children) {
            rows += SkillTreeRow(path, path.substringAfterLast('/'), depth, folder, removed)
            if (folder && path !in collapsed) visit(path, depth + 1)
        }
    }
    visit("", 0)
    return rows
}
