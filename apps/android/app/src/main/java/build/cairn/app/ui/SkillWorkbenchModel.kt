package build.cairn.app.ui

import android.content.ContentResolver
import android.net.Uri
import android.provider.OpenableColumns
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import build.cairn.app.data.*
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** The skill shown by the workbench; [create] opens an unsaved new skill. */
internal data class SkillTarget(val scope: String, val name: String, val create: Boolean = false) {
    val key: String
        get() = listOf(if (create) "new" else "open", scope, name).joinToString("\n")

    companion object {
        fun parse(key: String): SkillTarget? {
            val parts = key.split('\n')
            if (parts.size != 3) return null
            return SkillTarget(parts[1], parts[2], parts[0] == "new")
        }
    }
}

/**
 * Holds the edited skill across configuration changes. All edits stay in [draft] until [save] sends
 * one batch based on the revision the editor started from.
 */
internal class SkillWorkbenchModel : ViewModel() {
    @Volatile private var editorSession = 0L

    var target by mutableStateOf<SkillTarget?>(null)
        private set

    var draft by mutableStateOf<SkillDraft?>(null)
    var loading by mutableStateOf(false)
        private set

    /** File open in the editor. */
    var selected by mutableStateOf<String?>(null)
    var collapsed by mutableStateOf(emptySet<String>())
    var fileLoading by mutableStateOf(emptySet<String>())
        private set

    /** Non-null while saving, staging or exporting; describes the current step. */
    var progress by mutableStateOf<String?>(null)
        private set

    var conflict by mutableStateOf<String?>(null)
        private set

    var error by mutableStateOf<String?>(null)
    var source by mutableStateOf<SkillSourceStatus?>(null)
        private set

    var sourceLoading by mutableStateOf(false)
        private set

    val busy: Boolean
        get() = loading || progress != null

    fun open(vm: CairnViewModel, next: SkillTarget) {
        if (target == next) return
        reset()
        target = next
        if (next.create) {
            draft = SkillDraft.create(next.name)
            return
        }
        load(vm)
    }

    fun reset() {
        editorSession += 1
        target = null
        draft = null
        selected = null
        collapsed = emptySet()
        fileLoading = emptySet()
        progress = null
        conflict = null
        error = null
        source = null
        sourceLoading = false
        loading = false
    }

    /** Fetches the saved bundle again, dropping local edits. */
    fun load(vm: CairnViewModel) {
        val current = target ?: return
        loading = true
        conflict = null
        error = null
        val session = editorSession
        viewModelScope.launch {
            try {
                val bundle = vm.api.skillBundle(current.scope, current.name)
                if (editorSession != session || target != current) return@launch
                draft = SkillDraft.of(bundle)
                if (selected != null && draft?.file(selected!!) == null) selected = null
                ensureLoaded(vm, SKILL_FILE)
                selected?.let { ensureLoaded(vm, it) }
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                fail(vm, e)
            } finally {
                if (editorSession == session) loading = false
            }
        }
    }

    /** Loads the saved content of [path] once, so it can be shown and edited. */
    fun ensureLoaded(vm: CairnViewModel, path: String) {
        val current = target ?: return
        val file = draft?.file(path) ?: return
        val origin = file.origin ?: return
        if (file.body != null || draft?.originals?.containsKey(origin) == true) return
        if (origin in fileLoading) return
        fileLoading = fileLoading + origin
        val session = editorSession
        viewModelScope.launch {
            try {
                val content = vm.api.skillFile(current.scope, current.name, origin)
                val body = withContext(Dispatchers.Default) { SkillBody.of(content) }
                if (editorSession == session && target == current)
                    draft = draft?.withOriginal(origin, body)
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                fail(vm, e)
            } finally {
                if (editorSession == session) fileLoading = fileLoading - origin
            }
        }
    }

    /** Applies a local edit; returns the problem in French when it is refused. */
    fun update(change: (SkillDraft) -> SkillDraft): String? {
        if (busy) return "Attendez la fin de l’enregistrement."
        val current = draft ?: return null
        return try {
            draft = change(current)
            null
        } catch (e: IllegalArgumentException) {
            e.message
        } catch (e: IllegalStateException) {
            e.message
        }
    }

    fun save(vm: CairnViewModel, saved: (SkillTarget) -> Unit) {
        val current = draft ?: return
        val opened = target ?: return
        if (busy) return
        current.problems().firstOrNull()?.let {
            error = it
            return
        }
        conflict = null
        error = null
        progress = "Enregistrement…"
        val session = editorSession
        viewModelScope.launch {
            try {
                val bundle =
                    vm.api.applySkillChanges(
                        opened.scope,
                        opened.name,
                        current.base?.revision,
                        current.changes(),
                    ) {
                        if (editorSession == session)
                            progress = "Envoi de ${it.substringAfterLast('/')}…"
                    }
                if (editorSession != session) return@launch
                val known =
                    current.files
                        .mapNotNull { file -> current.content(file.path)?.let { file.path to it } }
                        .toMap()
                val next = opened.copy(create = false)
                target = next
                draft = SkillDraft.of(bundle).copy(originals = known)
                saved(next)
                vm.notify(if (opened.create) "Skill créé" else "Skill enregistré")
                try {
                    vm.refresh()
                } catch (e: Exception) {
                    if (editorSession != session) return@launch
                    if (e is CancellationException) throw e
                }
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                if (e is ApiException && e.status == 409)
                    conflict =
                        if (current.base == null) "Un skill porte déjà ce nom dans cette portée."
                        else "Ce skill a été modifié ailleurs depuis son ouverture."
                else fail(vm, e)
            } finally {
                if (editorSession == session) progress = null
            }
        }
    }

    /** Streams the saved archive into a document chosen with the Storage Access Framework. */
    fun export(vm: CairnViewModel, resolver: ContentResolver, uri: Uri) {
        val current = target ?: return
        if (busy) return
        progress = "Préparation de l’archive…"
        val session = editorSession
        viewModelScope.launch {
            try {
                val archive =
                    withContext(Dispatchers.IO) {
                        requireNotNull(resolver.openOutputStream(uri, "wt")) {
                                "Impossible d’écrire ce fichier."
                            }
                            .use { output ->
                                vm.api.downloadSkillArchive(
                                    current.scope,
                                    current.name,
                                    { output.write(it) },
                                ) { done, total ->
                                    if (editorSession == session)
                                        progress =
                                            "Téléchargement… ${if (total > 0) done * 100 / total else 0} %"
                                }
                            }
                    }
                if (editorSession != session) return@launch
                vm.notify("Archive ${archive.filename} enregistrée (${formatBytes(archive.size)})")
            } catch (e: Exception) {
                runCatching {
                    android.provider.DocumentsContract.deleteDocument(resolver, uri)
                }
                if (editorSession != session) return@launch
                if (e is ApiException && e.status == 409)
                    error = "Le skill a changé pendant le téléchargement. Réessayez."
                else fail(vm, e)
            } finally {
                if (editorSession == session) progress = null
            }
        }
    }

    /** Adds device documents under [folder] (root when empty); existing paths are replaced. */
    fun addDocuments(
        vm: CairnViewModel,
        resolver: ContentResolver,
        uris: List<Uri>,
        folder: String,
    ) {
        val limit = draft?.limits?.fileBytes ?: return
        val session = editorSession
        viewModelScope.launch {
            val problems = mutableListOf<String>()
            for (uri in uris) {
                try {
                    val (name, bytes) = readDocument(resolver, uri, limit)
                    val path = if (folder.isEmpty()) name else "$folder/$name"
                    val body = withContext(Dispatchers.Default) { SkillBody.of(bytes) }
                    if (editorSession != session) return@launch
                    update { it.put(path, body) }?.let { problems += "$name : $it" }
                    if (problems.isEmpty() && uris.size == 1) selected = path
                } catch (e: Exception) {
                    if (editorSession != session) return@launch
                    if (e is CancellationException) throw e
                    problems += e.message ?: "Lecture impossible."
                }
            }
            error = problems.firstOrNull()
        }
    }

    fun replaceDocument(vm: CairnViewModel, resolver: ContentResolver, path: String, uri: Uri) {
        val limit = draft?.limits?.fileBytes ?: return
        val session = editorSession
        viewModelScope.launch {
            try {
                val (_, bytes) = readDocument(resolver, uri, limit)
                val body = withContext(Dispatchers.Default) { SkillBody.of(bytes) }
                if (editorSession != session) return@launch
                error = update { it.put(path, body) }
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                if (e is CancellationException) throw e
                error = e.message ?: "Lecture impossible."
            }
        }
    }

    fun checkSource(vm: CairnViewModel) {
        val current = target ?: return
        sourceLoading = true
        val session = editorSession
        viewModelScope.launch {
            try {
                val result = vm.api.skillSource(current.scope, current.name)
                if (editorSession != session) return@launch
                source = result
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                fail(vm, e)
            } finally {
                if (editorSession == session) sourceLoading = false
            }
        }
    }

    fun updateSource(vm: CairnViewModel, commit: String, done: () -> Unit) {
        val current = target ?: return
        val base = draft?.base?.revision ?: return
        if (busy) return
        progress = "Mise à jour depuis GitHub…"
        val session = editorSession
        viewModelScope.launch {
            try {
                val bundle = vm.api.updateSkillSource(current.scope, current.name, commit, base)
                if (editorSession != session) return@launch
                draft = SkillDraft.of(bundle)
                source = null
                selected = selected?.takeIf { draft?.file(it) != null }
                ensureLoaded(vm, SKILL_FILE)
                selected?.let { ensureLoaded(vm, it) }
                done()
                vm.notify("Skill mis à jour depuis GitHub")
                runCatching { vm.refresh() }
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                if (e is ApiException && e.status == 409)
                    conflict = "Ce skill a été modifié ailleurs depuis son ouverture."
                else fail(vm, e)
            } finally {
                if (editorSession == session) progress = null
            }
        }
    }

    fun detachSource(vm: CairnViewModel, done: () -> Unit) {
        val current = target ?: return
        if (busy) return
        progress = "Détachement…"
        val session = editorSession
        viewModelScope.launch {
            try {
                vm.api.detachSkillSource(current.scope, current.name)
                if (editorSession != session) return@launch
                draft = draft?.let { it.copy(base = it.base?.copy(source = null)) }
                source = null
                done()
                vm.notify("Skill détaché de GitHub")
                runCatching { vm.refresh() }
            } catch (e: Exception) {
                if (editorSession != session) return@launch
                fail(vm, e)
            } finally {
                if (editorSession == session) progress = null
            }
        }
    }

    private fun fail(vm: CairnViewModel, e: Exception) {
        if (e is CancellationException) throw e
        if (e is ApiException && e.status == 401) vm.report(e)
        else error = e.message ?: "Connexion impossible. Réessayez."
    }
}

/** Display name and bytes of a document, refusing more than [limit] bytes. */
internal suspend fun readDocument(
    resolver: ContentResolver,
    uri: Uri,
    limit: Long,
): Pair<String, ByteArray> =
    withContext(Dispatchers.IO) {
        val name =
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                if (it.moveToFirst()) it.getString(0) else null
            } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "fichier"
        val clean = name.replace('/', '-').replace('\\', '-').trim()
        val bytes =
            requireNotNull(resolver.openInputStream(uri)) { "Impossible de lire $clean." }
                .use { input ->
                    val output = java.io.ByteArrayOutputStream()
                    val buffer = ByteArray(65536)
                    while (true) {
                        val count = input.read(buffer)
                        if (count < 0) break
                        require(output.size() + count <= limit) {
                            "$clean dépasse ${formatBytes(limit)}."
                        }
                        output.write(buffer, 0, count)
                    }
                    output.toByteArray()
                }
        clean to bytes
    }
