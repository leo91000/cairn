package build.cairn.app.data

import build.cairn.contracts.*
import java.util.Base64
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

/*
 * Multi-file skill bundles (docs/SKILLS.md): wire models, the batch change format and the
 * transfer helpers shared by the workbench, the import sheet and the archive export.
 */

const val MIB = 1024L * 1024

typealias SkillSource = build.cairn.contracts.SkillSource

typealias SkillFile = build.cairn.contracts.SkillFile

typealias SkillLimits = build.cairn.contracts.SkillLimits

typealias SkillBundle = build.cairn.contracts.SkillBundle

typealias SkillFileContent = build.cairn.contracts.SkillFileContent

typealias SkillUpload = build.cairn.contracts.SkillUpload

typealias SkillImport = build.cairn.contracts.SkillImport

typealias SkillArchiveChunk = build.cairn.contracts.SkillArchiveChunk

typealias SkillDiffLine = build.cairn.contracts.SkillDiffLine

typealias SkillSourceChange = build.cairn.contracts.SkillSourceChange

typealias SkillSourceVersion = build.cairn.contracts.SkillSourceVersion

typealias SkillSourceStatus = build.cairn.contracts.SkillSourceStatus

val SkillSource.shortVersion: String
    get() = shortVersion(version)
val SkillSource.reference: String
    get() =
        listOf(label.ifBlank { pluginId }, shortVersion)
            .filter { it.isNotBlank() }
            .joinToString("@")

fun shortVersion(version: String) = version.take(7)

fun SkillFileContent.bytes(): ByteArray =
    if (encoding == "base64") Base64.getDecoder().decode(content)
    else content.toByteArray(Charsets.UTF_8)

/** One operation of `POST /skills/:scope/:name/bundle`, applied in order by the server. */
sealed interface DraftSkillChange {
    data class Write(
        val path: String,
        val body: SkillBody,
        val executable: Boolean? = null,
        val upload: String? = null,
    ) : DraftSkillChange

    data class Delete(val path: String) : DraftSkillChange

    data class Move(val from: String, val to: String) : DraftSkillChange

    data class Mode(val path: String, val executable: Boolean) : DraftSkillChange
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

fun DraftSkillChange.toContract(): build.cairn.contracts.SkillChange =
    when (this) {
        is DraftSkillChange.Write ->
            build.cairn.contracts.SkillChange.A(
                path = path,
                content =
                    if (upload != null) null
                    else
                        when (val body = body) {
                            is SkillBody.Text -> body.value
                            is SkillBody.Binary -> Base64.getEncoder().encodeToString(body.bytes)
                        },
                encoding = if (upload == null && body is SkillBody.Binary) "base64" else null,
                upload = upload,
                executable = executable,
            )
        is DraftSkillChange.Delete -> build.cairn.contracts.SkillChange.B(path = path)
        is DraftSkillChange.Move -> build.cairn.contracts.SkillChange.C(from = from, to = to)
        is DraftSkillChange.Mode ->
            build.cairn.contracts.SkillChange.D(path = path, executable = executable)
    }

fun DraftSkillChange.toJson(): JsonObject =
    contractJson.encodeToJsonElement<build.cairn.contracts.SkillChange>(toContract()).jsonObject

/**
 * Keep small text inline and stage binaries or larger text with `POST /skills/uploads`.
 * Metadata-heavy batches may exceed this target within the 8 MB transport limit.
 */
const val SKILL_REQUEST_BUDGET = 120_000L

/** Encoded JSON size, including UTF-8 paths and every escaped control character. */
fun inlineCost(change: DraftSkillChange): Long =
    change.toJson().toString().toByteArray(Charsets.UTF_8).size.toLong()

/**
 * Paths whose content is staged before the batch: every binary file, then the largest text files
 * until the inline request fits in [budget].
 */
fun stagedWrites(
    changes: List<DraftSkillChange>,
    budget: Long = SKILL_REQUEST_BUDGET,
): Set<String> {
    val writes = changes.filterIsInstance<DraftSkillChange.Write>()
    val staged = writes.filter { it.body is SkillBody.Binary }.map { it.path }.toMutableSet()
    val stagedCost = 64L
    var total = changes.sumOf {
        if (it is DraftSkillChange.Write && it.path in staged) stagedCost + it.path.length
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
    readSkillBundle(SkillInput(scope = scope, name = name))

suspend fun CairnApi.skillFile(scope: String, name: String, path: String): SkillFileContent =
    readSkillFile(ReadSkillFileInput(scope = scope, name = name, path = path))

/** Stages raw bytes (at most `limits.uploadBytes`) for a batch or an archive import. */
suspend fun CairnApi.stageSkillUpload(bytes: ByteArray): SkillUpload =
    wireJson.decodeFromString(postBytes("/skills/uploads", bytes))

/** Stages what must be staged, then sends every change in one batch. */
suspend fun CairnApi.applyDraftSkillChanges(
    scope: String,
    name: String,
    base: String?,
    changes: List<DraftSkillChange>,
    progress: (String) -> Unit = {},
): SkillBundle {
    val writes = changes.filterIsInstance<DraftSkillChange.Write>()
    val staged = stagedWrites(changes)
    val uploads = mutableMapOf<String, String>()
    for (write in writes.filter { it.path in staged }) {
        progress(write.path)
        uploads[write.path] = stageSkillUpload(write.body.bytes()).id
    }
    val prepared = changes.map { change ->
        if (change is DraftSkillChange.Write && change.path in uploads)
            change.copy(upload = uploads.getValue(change.path))
        else change
    }
    return editSkillFiles(
        EditSkillFilesInput(
            scope = scope,
            name = name,
            base = base,
            changes = prepared.map { it.toContract() },
        )
    )
}

/** Imports from a staged archive; a chosen folder reuses the same upload. */
suspend fun CairnApi.importSkill(scope: String, upload: String, path: String? = null): SkillImport =
    importSkill(
        ImportSkillInput.A(scope = scope, upload = upload, path = path?.takeIf { it.isNotBlank() })
    )

suspend fun CairnApi.skillSource(scope: String, name: String): SkillSourceStatus =
    getSkillSourceStatus(SkillInput(scope = scope, name = name))

suspend fun CairnApi.updateSkillSource(
    scope: String,
    name: String,
    version: String,
    base: String,
): SkillBundle =
    updateSkillSource(
        UpdateSkillSourceInput(scope = scope, name = name, version = version, base = base)
    )

suspend fun CairnApi.detachSkillSource(scope: String, name: String) {
    forgetSkillSource(SkillInput(scope = scope, name = name))
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
            exportSkillArchive(
                ExportSkillArchiveInput(
                    scope = scope,
                    name = name,
                    offset = offset,
                    revision = revision,
                )
            )
        },
        write,
        progress,
    )
