package dev.leo.manager.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * An overlay keeps the reading viewport and composer stable when follow is paused. It sits centred
 * at the bottom of the history, right above the composer.
 */
@Composable
internal fun BoxScope.HistoryBottomButton(
    list: LazyListState,
    following: Boolean,
    label: String,
    onClick: () -> Unit,
) {
    val visible by
        remember(list, following) {
            derivedStateOf { !following && list.canScrollForward && !list.isScrollInProgress }
        }
    AnimatedVisibility(
        visible = visible,
        modifier = Modifier.align(Alignment.BottomCenter).padding(8.dp),
        enter = fadeIn(tween(durationMillis = 180, delayMillis = 120)),
        exit = fadeOut(tween(durationMillis = 120)),
    ) {
        IconButton(onClick = onClick, enabled = visible, modifier = Modifier.size(48.dp)) {
            Surface(
                modifier = Modifier.size(36.dp),
                shape = CircleShape,
                color = MaterialTheme.colorScheme.surfaceContainerHigh,
                contentColor = MaterialTheme.colorScheme.onSurface,
                border = BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
                shadowElevation = 2.dp,
            ) {
                Box(contentAlignment = Alignment.Center) {
                    Icon(LeoIcons.Bottom, label, Modifier.size(20.dp))
                }
            }
        }
    }
}
