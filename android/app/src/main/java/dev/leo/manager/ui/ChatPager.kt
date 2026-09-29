package dev.leo.manager.ui

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
import dev.leo.manager.data.*
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * The route identifies the starting chat; the pager owns subsequent selection and its restoration.
 */
@Composable
fun ChatScreen(
    vm: LeoViewModel,
    state: Workspace,
    id: String?,
    initialAgent: String = MAIN_AGENT_ID,
    initialProject: String = "",
    openChat: (String) -> Unit,
    openRun: (String) -> Unit,
    back: () -> Unit = {},
    create: () -> Unit = back,
    openConnections: () -> Unit = {},
) {
    val conversations = rememberLive(vm, state, "/chats/stream")
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
        )
        return
    }
    key(id) {
        var ids by rememberSaveable { mutableStateOf(listOf(id)) }
        val pager = rememberPagerState { ids.size }
        val scope = rememberCoroutineScope()
        val focus = LocalFocusManager.current
        val liveIds =
            remember(conversations.state?.chats) {
                conversations.state
                    ?.chats
                    ?.filter { it.lifecycle == "active" }
                    ?.sortedByDescending { it.updatedAt }
                    ?.map { it.id }
            }
        LaunchedEffect(liveIds) {
            val available = liveIds ?: return@LaunchedEffect
            // Activity can reorder the list while a finger is down. Keep both visible pages
            // fixed until settling, then preserve the selected chat by identity.
            snapshotFlow { pager.isScrollInProgress }.first { !it }
            val current = ids[pager.settledPage]
            val updated = if (current in available) available else listOf(current)
            if (updated != ids) {
                ids = updated
                pager.requestScrollToPage(updated.indexOf(current))
            }
        }
        val selectedId = ids.getOrNull(pager.settledPage)
        LaunchedEffect(selectedId) { focus.clearFocus() }
        HorizontalPager(
            state = pager,
            modifier =
                Modifier.fillMaxSize()
                    .background(MaterialTheme.colorScheme.background)
                    .testTag("conversation-pager"),
            key = { ids[it] },
            beyondViewportPageCount = 1,
            userScrollEnabled = ids.size > 1,
        ) { page ->
            val chatId = ids[page]
            val selected = page == pager.settledPage
            Box(
                Modifier.fillMaxSize().testTag("chat-page:$chatId").semantics {
                    this.selected = selected
                }
            ) {
                // Preloaded pages render real content but cannot take keyboard or TalkBack focus.
                Box(
                    Modifier.fillMaxSize()
                        .focusProperties { canFocus = selected }
                        .then(if (selected) Modifier else Modifier.clearAndSetSemantics {})
                ) {
                    ChatPage(
                        vm,
                        state,
                        chatId,
                        conversations,
                        selected = selected,
                        openChat = { target ->
                            val index = ids.indexOf(target)
                            if (index < 0) openChat(target)
                            else scope.launch { pager.scrollToPage(index) }
                        },
                        openRun = openRun,
                        back = back,
                        create = create,
                        openConnections = openConnections,
                    )
                }
            }
        }
    }
}
