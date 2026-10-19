package build.cairn.app.data

import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive

/** Owner management reads complete every page before exposing the saved list. */
internal suspend inline fun <reified T> CairnApi.pluginManagementItems(path: String): List<T> {
    val items = mutableListOf<T>()
    val cursors = mutableSetOf<String>()
    var cursor: String? = null
    do {
        currentCoroutineContext().ensureActive()
        val target = cursor?.let { "$path?cursor=${segment(it)}" } ?: path
        val page = get<PluginPage<T>>(target)
        currentCoroutineContext().ensureActive()
        items += page.items
        cursor = page.nextCursor
        check(cursor == null || cursors.add(cursor)) {
            "La liste a changé pendant le chargement. Actualisez-la avant une modification."
        }
    } while (cursor != null)
    return items
}
