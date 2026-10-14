package build.cairn.app.data

import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/*
 * Multi-file skill bundles (docs/SKILLS.md): wire models, the batch change format and the
 * transfer helpers shared by the workbench, the import sheet and the archive export.
 */

const val MIB = 1024L * 1024

@Serializable
data class SkillSource(
    val type: String = "github",
    val repository: String = "",
    val path: String = "",
    val ref: String = "",
    val commit: String = "",
    val importedAt: Long = 0,
    val modified: Boolean = false,
) {
    /** `owner/repo@9b1e2c4`, the compact reference shown on cards. */
    val label: String
        get() = if (commit.isBlank()) repository else "$repository@${commit.take(7)}"
}

@Serializable
data class SkillFile(
    val path: String,
    val size: Long = 0,
    val executable: Boolean = false,
    val text: Boolean = true,
)

@Serializable
data class SkillLimits(
    val fileBytes: Long = 4 * MIB,
    val bundleBytes: Long = 16 * MIB,
    val files: Int = 500,
    val uploadBytes: Long = 5 * MIB,
)

@Serializable
data class SkillBundle(
    val scope: String = "global",
    val name: String = "",
    val revision: String = "",
    val files: List<SkillFile> = emptyList(),
    val limits: SkillLimits = SkillLimits(),
    val source: SkillSource? = null,
    val description: String = "",
)

@Serializable
data class SkillFileContent(
    val content: String = "",
    val encoding: String = "utf-8",
    val size: Long = 0,
    val executable: Boolean = false,
) {
    fun bytes(): ByteArray =
        if (encoding == "base64") Base64.getDecoder().decode(content)
        else content.toByteArray(Charsets.UTF_8)
}

@Serializable data class SkillUpload(val id: String, val size: Long = 0)

@Serializable
data class SkillImport(
    val imported: SkillBundle? = null,
    val candidates: List<String> = emptyList(),
)

@Serializable
data class SkillArchiveChunk(
    val filename: String = "skill.zip",
    val revision: String = "",
    val size: Long = 0,
    val offset: Long = 0,
    val content: String = "",
)

@Serializable data class SkillDiffLine(val kind: String = "context", val text: String = "")

@Serializable
data class SkillSourceChange(
    val path: String,
    val status: String = "modified",
    val binary: Boolean = false,
    val lines: List<SkillDiffLine>? = null,
)

@Serializable
data class SkillSourceCommit(
    val commit: String = "",
    val message: String = "",
    val date: String = "",
)

@Serializable
data class SkillSourceStatus(
    val source: SkillSource = SkillSource(),
    val latest: SkillSourceCommit = SkillSourceCommit(),
    val current: Boolean = true,
    val changes: List<SkillSourceChange> = emptyList(),
)

/** One operation of `POST /skills/:scope/:name/bundle`, applied in order by the server. */
sealed interface SkillChange {
    data class Write(
        val path: String,
        val body: SkillBody,
        val executable: Boolean? = null,
        val upload: String? = null,
    ) : SkillChange

    data class Delete(val path: String) : SkillChange

    data class Move(val from: String, val to: String) : SkillChange

    data class Mode(val path: String, val executable: Boolean) : SkillChange
}

/** File content held by the editor: text stays a string, everything else stays bytes. */
sealed interface SkillBody {
    val size: Long

    data class Text(val value: String) : SkillBody {
        override val size: Long
            get() = value.toByteArray(Charsets.UTF_8).size.toLong()
    }

    class Binary(val bytes: ByteArray) : SkillBody {
        override val size: Long
            get() = bytes.size.toLong()

        override fun equals(other: Any?) = other is Binary && bytes.contentEquals(other.bytes)

        override fun hashCode() = bytes.contentHashCode()
    }

    fun bytes(): ByteArray =
        when (this) {
            is Text -> value.toByteArray(Charsets.UTF_8)
            is Binary -> bytes
        }

    companion object {
        /** Text means valid UTF-8 without NUL bytes, as the server decides. */
        fun of(bytes: ByteArray): SkillBody {
            if (bytes.any { it == 0.toByte() }) return Binary(bytes)
            val decoder =
                Charsets.UTF_8.newDecoder()
                    .onMalformedInput(java.nio.charset.CodingErrorAction.REPORT)
                    .onUnmappableCharacter(java.nio.charset.CodingErrorAction.REPORT)
            return try {
                Text(decoder.decode(java.nio.ByteBuffer.wrap(bytes)).toString())
            } catch (_: java.nio.charset.CharacterCodingException) {
                Binary(bytes)
            }
        }

        fun of(content: SkillFileContent): SkillBody =
            if (content.encoding == "base64") of(content.bytes()) else Text(content.content)
    }
}

fun SkillChange.toJson(): JsonObject = buildJsonObject {
    when (val change = this@toJson) {
        is SkillChange.Write -> {
            put("op", "write")
            put("path", change.path)
            if (change.upload != null) put("upload", change.upload)
            else
                when (val body = change.body) {
                    is SkillBody.Text -> put("content", body.value)
                    is SkillBody.Binary -> {
                        put("content", Base64.getEncoder().encodeToString(body.bytes))
                        put("encoding", "base64")
                    }
                }
            if (change.executable != null) put("executable", change.executable)
        }
        is SkillChange.Delete -> {
            put("op", "delete")
            put("path", change.path)
        }
        is SkillChange.Move -> {
            put("op", "move")
            put("from", change.from)
            put("to", change.to)
        }
        is SkillChange.Mode -> {
            put("op", "mode")
            put("path", change.path)
            put("executable", change.executable)
        }
    }
}

/**
 * Installation JSON bodies are capped at 150 KB: a batch keeps small text files inline and stages
 * every binary file and larger text with `POST /skills/uploads`.
 */
const val SKILL_REQUEST_BUDGET = 120_000L

/** Approximate JSON size of a change sent inline, including its keys and path. */
fun inlineCost(change: SkillChange): Long {
    val overhead = 48L
    return when (change) {
        is SkillChange.Write ->
            overhead +
                change.path.length +
                when (val body = change.body) {
                    is SkillBody.Text ->
                        body.size +
                            body.value.count { it == '"' || it == '\\' || it == '\n' || it < ' ' }
                    is SkillBody.Binary -> (body.size + 2) / 3 * 4
                }
        is SkillChange.Delete -> overhead + change.path.length
        is SkillChange.Mode -> overhead + change.path.length
        is SkillChange.Move -> overhead + change.from.length + change.to.length
    }
}

/**
 * Paths whose content is staged before the batch: every binary file, then the largest text files
 * until the inline request fits in [budget].
 */
fun stagedWrites(changes: List<SkillChange>, budget: Long = SKILL_REQUEST_BUDGET): Set<String> {
    val writes = changes.filterIsInstance<SkillChange.Write>()
    val staged = writes.filter { it.body is SkillBody.Binary }.map { it.path }.toMutableSet()
    val stagedCost = 64L
    var total = changes.sumOf {
        if (it is SkillChange.Write && it.path in staged) stagedCost + it.path.length
        else inlineCost(it)
    }
    for (write in writes.filter { it.path !in staged }.sortedByDescending { inlineCost(it) }) {
        if (total <= budget) break
        staged += write.path
        total -= inlineCost(write) - stagedCost - write.path.length
    }
    return staged
}

fun skillRoute(scope: String, name: String) = "/skills/${segment(scope)}/${segment(name)}"

suspend fun CairnApi.skillBundle(scope: String, name: String): SkillBundle =
    get("${skillRoute(scope, name)}/bundle")

suspend fun CairnApi.skillFile(scope: String, name: String, path: String): SkillFileContent =
    get("${skillRoute(scope, name)}/file?path=${segment(path)}")

/** Stages raw bytes (at most `limits.uploadBytes`) for a batch or an archive import. */
suspend fun CairnApi.stageSkillUpload(bytes: ByteArray): SkillUpload =
    wireJson.decodeFromString(postBytes("/skills/uploads", bytes))

/** Stages what must be staged, then sends every change in one batch. */
suspend fun CairnApi.applySkillChanges(
    scope: String,
    name: String,
    base: String?,
    changes: List<SkillChange>,
    progress: (String) -> Unit = {},
): SkillBundle {
    val writes = changes.filterIsInstance<SkillChange.Write>()
    val staged = stagedWrites(changes)
    val uploads = mutableMapOf<String, String>()
    for (write in writes.filter { it.path in staged }) {
        progress(write.path)
        uploads[write.path] = stageSkillUpload(write.body.bytes()).id
    }
    val prepared = changes.map { change ->
        if (change is SkillChange.Write && change.path in uploads)
            change.copy(upload = uploads.getValue(change.path))
        else change
    }
    return send(
        "POST",
        "${skillRoute(scope, name)}/bundle",
        buildJsonObject {
            put("base", base)
            put("changes", buildJsonArray { prepared.forEach { add(it.toJson()) } })
        },
    )
}

suspend fun CairnApi.importSkill(
    scope: String,
    upload: String? = null,
    repository: String? = null,
    path: String? = null,
    ref: String? = null,
): SkillImport =
    send(
        "POST",
        "/skills/${segment(scope)}/import",
        buildJsonObject {
            if (upload != null) {
                put("source", "archive")
                put("upload", upload)
            } else {
                put("source", "github")
                put("repository", repository.orEmpty())
                if (!ref.isNullOrBlank()) put("ref", ref)
            }
            if (!path.isNullOrBlank()) put("path", path)
        },
    )

suspend fun CairnApi.skillSource(scope: String, name: String): SkillSourceStatus =
    get("${skillRoute(scope, name)}/source")

suspend fun CairnApi.updateSkillSource(
    scope: String,
    name: String,
    commit: String,
    base: String,
): SkillBundle =
    send(
        "POST",
        "${skillRoute(scope, name)}/source/update",
        buildJsonObject {
            put("commit", commit)
            put("base", base)
        },
    )

suspend fun CairnApi.detachSkillSource(scope: String, name: String) {
    request("DELETE", "${skillRoute(scope, name)}/source")
}

class SkillArchive(val filename: String, val revision: String, val size: Long)

/**
 * Downloads `GET …/archive` chunk by chunk: the first request fixes the revision, the following
 * ones repeat it with `offset += decoded length` until the whole archive is written.
 */
suspend fun downloadSkillArchive(
    fetch: suspend (offset: Long, revision: String?) -> SkillArchiveChunk,
    write: (ByteArray) -> Unit,
    progress: (Long, Long) -> Unit = { _, _ -> },
): SkillArchive {
    var offset = 0L
    var revision: String? = null
    var first: SkillArchiveChunk? = null
    while (true) {
        val chunk = fetch(offset, revision)
        check(chunk.offset == offset) { "L’archive reçue est incohérente." }
        check(revision == null || chunk.revision == revision) {
            "Le skill a changé pendant le téléchargement. Réessayez."
        }
        revision = chunk.revision
        if (first == null) first = chunk
        val bytes = Base64.getDecoder().decode(chunk.content)
        check(offset + bytes.size <= chunk.size) { "L’archive reçue est incohérente." }
        check(bytes.isNotEmpty() || offset == chunk.size) { "L’archive reçue est incomplète." }
        if (bytes.isNotEmpty()) write(bytes)
        offset += bytes.size
        progress(offset, chunk.size)
        if (offset == chunk.size) return SkillArchive(first.filename, chunk.revision, chunk.size)
    }
}

suspend fun CairnApi.downloadSkillArchive(
    scope: String,
    name: String,
    write: (ByteArray) -> Unit,
    progress: (Long, Long) -> Unit = { _, _ -> },
): SkillArchive =
    downloadSkillArchive(
        { offset, revision ->
            get(
                "${skillRoute(scope, name)}/archive?offset=$offset" +
                    (revision?.let { "&revision=${segment(it)}" } ?: "")
            )
        },
        write,
        progress,
    )

/** What the GitHub import needs, read from `owner/repo` or a pasted GitHub URL. */
data class GithubSkillReference(val repository: String, val ref: String = "", val path: String = "")

private val githubName = Regex("[A-Za-z0-9_.-]+")

fun githubSkillReference(input: String): GithubSkillReference? {
    val text = input.trim().removeSuffix("/")
    val relative =
        text.removePrefix("https://").removePrefix("http://").removePrefix("www.").let {
            if (it.startsWith("github.com/")) it.removePrefix("github.com/")
            else if (it == text && !text.contains("://")) it else return null
        }
    val parts = relative.split('/').filter { it.isNotEmpty() }
    if (parts.size < 2) return null
    val owner = parts[0]
    val repository = parts[1].removeSuffix(".git")
    if (!githubName.matches(owner) || !githubName.matches(repository)) return null
    if (parts.size == 2) return GithubSkillReference("$owner/$repository")
    if (parts[2] !in setOf("tree", "blob") || parts.size < 4) return null
    val path = parts.drop(4).joinToString("/").removeSuffix("/$SKILL_FILE").removeSuffix(SKILL_FILE)
    return GithubSkillReference("$owner/$repository", parts[3], path)
}
