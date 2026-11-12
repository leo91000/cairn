package build.cairn.app.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import build.cairn.app.data.*
import build.cairn.app.diagnostics.PerfScreen
import build.cairn.app.diagnostics.Performance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * The route identifies the starting chat; the pager owns subsequent selection and its restoration.
 */
@Composable
fun ChatScreen(
    vm: CairnViewModel,
    state: Workspace,
    id: String?,
    initialAgent: String = MAIN_AGENT_ID,
    initialProject: String = "",
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    openFilRun: (String) -> Unit = openRun,
    back: () -> Unit = {},
    create: () -> Unit = back,
    openConnections: () -> Unit = {},
    openSettings: () -> Unit = {},
) {
    val conversations = rememberFilConversations(vm, state)
    if (id == null) {
        ChatPage(
            vm,
            state,
            null,
            conversations,
            initialAgent = initialAgent,
            initialProject = initialProject,
            openChat = openChat,
            openRun = openRun,
            back = back,
            create = create,
            openConnections = openConnections,
            openSettings = openSettings,
        )
        return
    }
    FilPager(
        vm,
        state,
        "chat:$id",
        conversations,
        openChat,
        openRun,
        openFilRun,
        back,
        create,
        openConnections,
        openSettings,
    )
}

/** A mission run opened from the Fil, read in the same pager as its conversations. */
@Composable
fun FilRunScreen(
    vm: CairnViewModel,
    state: Workspace,
    id: String,
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    openFilRun: (String) -> Unit,
    back: () -> Unit = {},
    create: () -> Unit = back,
    openConnections: () -> Unit = {},
    openSettings: () -> Unit = {},
) {
    val conversations = rememberFilConversations(vm, state)
    FilPager(
        vm,
        state,
        "run:$id",
        conversations,
        openChat,
        openRun,
        openFilRun,
        back,
        create,
        openConnections,
        openSettings,
    )
}

/**
 * The pages a swipe moves through, in the Fil's order. Once opened, the current page keeps its
 * position while read acknowledgements reorder the feed; one opened from elsewhere stays alone.
 */
internal fun pagerKeys(fil: List<String>, keys: List<String>, page: Int): List<String> {
    val current = keys[page]
    return when {
        keys.size > 1 ->
            (fil - current).toMutableList().apply { add(page.coerceAtMost(size), current) }
        current in fil -> fil
        else -> listOf(current)
    }
}

/**
 * Conversations and missions of the Fil side by side, keyed as the Fil keys them. The route
 * identifies the starting page; the pager owns subsequent selection and its restoration.
 */
@Composable
private fun FilPager(
    vm: CairnViewModel,
    state: Workspace,
    start: String,
    conversations: LiveSnapshot,
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    openFilRun: (String) -> Unit,
    back: () -> Unit,
    create: () -> Unit,
    openConnections: () -> Unit,
    openSettings: () -> Unit,
) {
    val activity = rememberFilActivity(vm)
    key(start) {
        var keys by rememberSaveable { mutableStateOf(listOf(start)) }
        val pager = rememberPagerState { keys.size }
        val scope = rememberCoroutineScope()
        val focus = LocalFocusManager.current
        LaunchedEffect(pager) {
            try {
                snapshotFlow { pager.isScrollInProgress }.collect { Performance.swiping = it }
            } finally {
                Performance.swiping = false
            }
        }
        val filKeys =
            remember(conversations.state?.chats, activity, state.tasks, state.usableAgents) {
                val chats = conversations.state?.chats
                if (chats == null || activity == null) null
                else
                    buildFeed(
                            chats.filter { it.lifecycle == "active" },
                            activity,
                            state.tasks,
                            usable = state.usableAgents,
                        )
                        .keys()
            }
        LaunchedEffect(filKeys) {
            val available = filKeys ?: return@LaunchedEffect
            // Activity can reorder the Fil while a finger is down. Keep both visible pages
            // fixed until settling, then preserve the selected page by identity.
            snapshotFlow { pager.isScrollInProgress }.first { !it }
            val current = keys[pager.settledPage]
            val updated = pagerKeys(available, keys, pager.settledPage)
            if (updated != keys) {
                keys = updated
                pager.requestScrollToPage(updated.indexOf(current))
            }
        }
        val selectedKey = keys.getOrNull(pager.settledPage)
        val reportScreen = LocalPerformanceScreen.current
        DisposableEffect(selectedKey) {
            reportScreen(
                if (selectedKey?.startsWith("run:") == true) PerfScreen.RUN else PerfScreen.CHAT
            )
            onDispose { reportScreen(null) }
        }
        LaunchedEffect(selectedKey) { focus.clearFocus() }
        fun show(target: String, elsewhere: () -> Unit) {
            val index = keys.indexOf(target)
            if (index < 0) elsewhere() else scope.launch { pager.scrollToPage(index) }
        }
        HorizontalPager(
            state = pager,
            modifier =
                Modifier.fillMaxSize()
                    .background(MaterialTheme.colorScheme.background)
                    .testTag("conversation-pager"),
            key = { keys[it] },
            beyondViewportPageCount = 1,
            userScrollEnabled = keys.size > 1,
        ) { page ->
            val pageKey = keys[page]
            val runId = pageKey.removePrefix("run:").takeIf { pageKey.startsWith("run:") }
            val selected = page == pager.settledPage
            Box(
                Modifier.fillMaxSize()
                    .testTag(
                        if (runId != null) "run-page:$runId"
                        else "chat-page:${pageKey.removePrefix("chat:")}"
                    )
                    .semantics { this.selected = selected }
            ) {
                // Preloaded pages render real content but cannot take keyboard or TalkBack focus.
                Box(
                    Modifier.fillMaxSize()
                        .focusProperties { canFocus = selected }
                        .then(if (selected) Modifier else Modifier.clearAndSetSemantics {})
                ) {
                    val showChat = { target: String -> show("chat:$target") { openChat(target) } }
                    if (runId != null)
                        RunScreen(
                            vm,
                            state,
                            runId,
                            openChat = showChat,
                            back = back,
                            selected = selected,
                            mission = true,
                        ) { target ->
                            show("run:$target") { openFilRun(target) }
                        }
                    else
                        ChatPage(
                            vm,
                            state,
                            pageKey.removePrefix("chat:"),
                            conversations,
                            selected = selected,
                            openChat = showChat,
                            openRun = openRun,
                            back = back,
                            create = create,
                            openConnections = openConnections,
                            openSettings = openSettings,
                        )
                }
            }
        }
    }
}
