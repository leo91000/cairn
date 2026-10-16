package build.cairn.app.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import build.cairn.app.data.CairnViewModel

/**
 * Reads finished work while it is on screen and the app is in the foreground: a turn that ends in
 * front of the user is never left unread, one that ended in the background waits for their return.
 * [item] is the Fil key read (`chat:<id>` or `task:<id>`) with the end of the work it reads.
 */
@Composable
internal fun ReadWhileOpen(vm: CairnViewModel, item: Pair<String, Long>?) {
    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
    val resumed = lifecycle.isAtLeast(Lifecycle.State.RESUMED)
    LaunchedEffect(item, resumed) { if (resumed && item != null) vm.markRead(listOf(item)) }
}
