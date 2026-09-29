package dev.leo.manager.ui

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animate
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.Orientation
import androidx.compose.foundation.gestures.draggable
import androidx.compose.foundation.gestures.rememberDraggableState
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import dev.leo.manager.data.*
import kotlin.math.roundToInt
import kotlinx.coroutines.launch
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** The app-wide snackbar, so screens can offer an undo without owning a Scaffold. */
internal val LocalSnackbar = staticCompositionLocalOf<SnackbarHostState?> { null }

/**
 * Conversations hidden while their deletion is pending or done, and the action that trashes one.
 */
internal class ConversationTrash(val hidden: Set<String>, val trash: (String) -> Unit)

/**
 * Trashes optimistically: the row leaves at once, comes back if the server refuses or the user
 * cancels the working-conversation confirmation, and an undo restores it for a few seconds.
 */
@Composable
internal fun rememberConversationTrash(
    vm: LeoViewModel,
    snackbar: SnackbarHostState? = LocalSnackbar.current,
    onChange: () -> Unit = {},
): ConversationTrash {
    var hidden by remember { mutableStateOf(setOf<String>()) }
    var confirming by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val change by rememberUpdatedState(onChange)
    fun restore(id: String) {
        vm.viewModelScope.launch {
            try {
                vm.api.request("POST", "/chats/${segment(id)}/restore")
                hidden = hidden - id
                change()
            } catch (e: Exception) {
                vm.report(e)
            }
        }
    }
    fun trashed(id: String) {
        vm.chatDrafts.remove(id)
        confirming = null
        change()
        if (snackbar != null)
            scope.launch {
                snackbar.currentSnackbarData?.dismiss()
                val result =
                    snackbar.showSnackbar(
                        "Conversation supprimée",
                        "Annuler",
                        duration = SnackbarDuration.Short,
                    )
                if (result == SnackbarResult.ActionPerformed) restore(id)
            }
    }
    fun trash(id: String) {
        hidden = hidden + id
        vm.viewModelScope.launch {
            try {
                vm.api.request(
                    "DELETE",
                    "/chats/${segment(id)}",
                    buildJsonObject { put("confirm", false) },
                )
                vm.historyCache.clear()
                trashed(id)
            } catch (e: Exception) {
                if (e is ApiException && e.status == 409) confirming = id
                else {
                    hidden = hidden - id
                    vm.report(e)
                }
            }
        }
    }
    confirming?.let { id ->
        val state by vm.state.collectAsStateWithLifecycle()
        Confirm(
            "Arrêter et supprimer ?",
            "Le travail sera arrêté et les envois annulés. La conversation restera récupérable pendant 30 jours.",
            state.busy,
            state.error,
            {
                confirming = null
                hidden = hidden - id
            },
        ) {
            vm.perform {
                api.request(
                    "DELETE",
                    "/chats/${segment(id)}",
                    buildJsonObject { put("confirm", true) },
                )
                historyCache.clear()
                trashed(id)
            }
        }
    }
    return ConversationTrash(hidden, ::trash)
}

private const val COMMIT_FRACTION = 0.4f

/**
 * Gmail-style swipe to trash: the row follows the finger over a red backdrop, springs back if
 * released early, and slides out once past the threshold or flung.
 */
@Composable
internal fun SwipeToTrashRow(
    enabled: Boolean = true,
    onTrash: () -> Unit,
    content: @Composable (Modifier) -> Unit,
) {
    val haptics = LocalHapticFeedback.current
    val fling = with(LocalDensity.current) { 900.dp.toPx() }
    val colors = MaterialTheme.colorScheme
    var width by remember { mutableIntStateOf(0) }
    var offset by remember { mutableFloatStateOf(0f) }
    fun armed() = width > 0 && offset <= -width * COMMIT_FRACTION
    val armed = armed()
    LaunchedEffect(armed) {
        if (armed) haptics.performHapticFeedback(HapticFeedbackType.GestureThresholdActivate)
    }
    val backdrop by
        animateColorAsState(
            if (armed) colors.error else colors.errorContainer,
            label = "trash-backdrop",
        )
    val iconScale by
        animateFloatAsState(
            if (armed) 1.25f else 0.85f,
            spring(Spring.DampingRatioMediumBouncy),
            label = "trash-icon",
        )
    Box(
        Modifier.fillMaxWidth()
            .onSizeChanged { width = it.width }
            .clip(RoundedCornerShape(16.dp))
            .semantics {
                if (enabled)
                    customActions =
                        listOf(
                            CustomAccessibilityAction("Supprimer") {
                                onTrash()
                                true
                            }
                        )
            }
    ) {
        if (offset < 0f)
            Box(
                Modifier.matchParentSize().background(backdrop).padding(end = 28.dp),
                contentAlignment = Alignment.CenterEnd,
            ) {
                Icon(
                    Icons.Default.Delete,
                    null,
                    Modifier.scale(iconScale),
                    tint = if (armed) colors.onError else colors.onErrorContainer,
                )
            }
        content(
            Modifier.offset { IntOffset(offset.roundToInt(), 0) }
                .then(
                    if (offset < 0f) Modifier.background(colors.surfaceContainerHigh) else Modifier
                )
                .draggable(
                    state =
                        rememberDraggableState {
                            offset = (offset + it).coerceIn(-width.toFloat(), 0f)
                        },
                    orientation = Orientation.Horizontal,
                    enabled = enabled,
                    onDragStopped = { velocity ->
                        val commit = armed() || (velocity < -fling && offset < -width * 0.1f)
                        animate(
                            offset,
                            if (commit) -width.toFloat() else 0f,
                            velocity,
                            if (commit) tween(160)
                            else spring(Spring.DampingRatioMediumBouncy, Spring.StiffnessMedium),
                        ) { value, _ ->
                            offset = value
                        }
                        if (commit) onTrash()
                    },
                )
        )
    }
}

/**
 * A swipe only reveals the explicit action; swiping right closes it. Used where removal must stay
 * deliberate.
 */
@Composable
internal fun SwipeToRevealRow(
    enabled: Boolean = true,
    label: String,
    onDelete: () -> Unit,
    content: @Composable (Modifier) -> Unit,
) {
    var revealed by remember { mutableStateOf(false) }
    var drag by remember { mutableFloatStateOf(0f) }
    val distance = with(LocalDensity.current) { 104.dp.roundToPx() }
    val threshold = with(LocalDensity.current) { 32.dp.toPx() }
    Box(Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp))) {
        if (revealed && enabled)
            TextButton(
                onClick = onDelete,
                modifier = Modifier.align(Alignment.CenterEnd).width(104.dp),
            ) {
                Text(label, color = MaterialTheme.colorScheme.error)
            }
        content(
            Modifier.offset { IntOffset(if (revealed && enabled) -distance else 0, 0) }
                .draggable(
                    state = rememberDraggableState { drag += it },
                    orientation = Orientation.Horizontal,
                    enabled = enabled,
                    onDragStarted = { drag = 0f },
                    onDragStopped = {
                        if (drag < -threshold) revealed = true
                        else if (drag > threshold) revealed = false
                    },
                )
        )
    }
}
