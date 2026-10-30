package build.cairn.app.data

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import build.cairn.contracts.InstallPluginPackageInput
import build.cairn.contracts.Modules
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonPrimitive

/** Native folder selection supplies the same manifest and Luau files as web uploads. */
suspend fun readPluginPackage(context: Context, tree: Uri): InstallPluginPackageInput =
    withContext(Dispatchers.IO) {
        val files = linkedMapOf<String, String>()
        var bytes = 0
        var entries = 0
        fun visit(document: String, prefix: String, depth: Int) {
            require(depth <= 4) { "Le package contient trop de sous-dossiers." }
            val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, document)
            context.contentResolver
                .query(
                    children,
                    arrayOf(
                        DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                        DocumentsContract.Document.COLUMN_MIME_TYPE,
                    ),
                    null,
                    null,
                    null,
                )
                ?.use { cursor ->
                    while (cursor.moveToNext()) {
                        require(++entries <= 256) { "Le dossier contient trop de fichiers." }
                        val id = cursor.getString(0)
                        val name = prefix + cursor.getString(1)
                        if (cursor.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR)
                            visit(id, "$name/", depth + 1)
                        else if (
                            name in listOf("manifest.json", "manifest.toml") ||
                                name.endsWith(".luau")
                        ) {
                            val uri = DocumentsContract.buildDocumentUriUsingTree(tree, id)
                            val content =
                                requireNotNull(context.contentResolver.openInputStream(uri)).use {
                                    readPluginFile(it)
                                }
                            bytes += content.size
                            require(bytes <= 458_752) { "Le package est trop volumineux." }
                            files[name] = content.toString(Charsets.UTF_8)
                        }
                    }
                } ?: error("Ce dossier est inaccessible.")
        }
        visit(DocumentsContract.getTreeDocumentId(tree), "", 0)
        pluginPackageInput(files)
    }

internal fun pluginPackageInput(files: Map<String, String>): InstallPluginPackageInput {
    val manifests = files.keys.filter { it in listOf("manifest.json", "manifest.toml") }
    require(manifests.size == 1 && "main.luau" in files) {
        "Choisissez un dossier contenant un seul manifest.json ou manifest.toml et main.luau."
    }
    val manifest = manifests.single()
    return InstallPluginPackageInput(
        manifest = JsonPrimitive(files.getValue(manifest)),
        manifestFormat = if (manifest.endsWith(".toml")) "toml" else "json",
        source = files.getValue("main.luau"),
        modules = Modules(files.filterKeys { it.endsWith(".luau") && it != "main.luau" }),
    )
}

internal fun readPluginFile(input: java.io.InputStream): ByteArray {
    val output = java.io.ByteArrayOutputStream()
    val buffer = ByteArray(8192)
    while (true) {
        val size = input.read(buffer)
        if (size < 0) break
        require(output.size() + size <= 458_752) { "Le package est trop volumineux." }
        output.write(buffer, 0, size)
    }
    return output.toByteArray()
}
