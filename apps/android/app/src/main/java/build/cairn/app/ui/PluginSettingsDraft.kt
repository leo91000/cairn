package build.cairn.app.ui

import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.Saver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.platform.LocalContext
import build.cairn.app.data.*
import java.util.UUID

/** Ordinary setting inputs only. Credentials and OAuth consent never enter this store. */
internal class PluginSettingsDraft(
    values: Map<String, String>,
    internal val key: String = UUID.randomUUID().toString(),
    val notice: String? = null,
    private val remove: (String) -> Unit,
) {
    val state = mutableStateOf(values)

    internal var discarded = false

    fun discard() {
        discarded = true
        remove(key)
    }
}

/** Saved instance state holds an opaque reference; larger form inputs stay device-local. */
@Composable
internal fun rememberPluginSettingsDraft(
    fields: List<PluginSetting>,
    initial: Map<String, String>,
    scope: String,
): PluginSettingsDraft {
    val context = LocalContext.current.applicationContext
    val allowed = fields.filter { it.type != "secret" }.map { it.key }.toSet()
    val store = remember(scope, allowed) { PluginSettingsDraftStore(context, scope, allowed) }
    val saver =
        remember(store) {
            Saver<PluginSettingsDraft, String>(
                save = {
                    if (!it.discarded && store.write(it.key, it.state.value)) it.key else "lost"
                },
                restore = { key ->
                    val restored = store.read(key)
                    PluginSettingsDraft(
                        restored ?: initial.filterKeys { it in allowed },
                        if (store.valid(key)) key else UUID.randomUUID().toString(),
                        if (restored == null)
                            "Les saisies locales n’ont pas pu être récupérées. Les réglages enregistrés ont été rechargés."
                        else null,
                        store::remove,
                    )
                },
            )
        }
    return rememberSaveable(scope, saver = saver) {
        PluginSettingsDraft(initial.filterKeys { it in allowed }, remove = store::remove)
    }
}
