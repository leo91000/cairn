package build.cairn.app.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp

/** A handle owns its drag; scrolling elsewhere and the accessible move buttons stay available. */
@Composable
internal fun NodePriorityList(
    ids: List<String>,
    name: (String) -> String,
    enabled: Boolean,
    reorder: (List<String>) -> Unit,
    remove: (String) -> Unit,
) {
    val bounds = remember { mutableMapOf<String, Rect>() }
    var dragged by remember(ids, enabled) { mutableStateOf<String?>(null) }
    var target by remember(ids, enabled) { mutableStateOf<String?>(null) }
    val latestReorder by rememberUpdatedState(reorder)
    Text(
        "Glissez une poignée sur une autre node, ou utilisez Monter et Descendre.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    ids.forEachIndexed { index, id ->
        key(id) {
            val label = name(id)
            var handleTop by remember { mutableFloatStateOf(0f) }
            var pointerY by remember { mutableFloatStateOf(0f) }
            val highlight = target == id && dragged != id
            Column(
                Modifier.fillMaxWidth()
                    .onGloballyPositioned { bounds[id] = it.boundsInRoot() }
                    .background(
                        if (highlight) MaterialTheme.colorScheme.secondaryContainer
                        else MaterialTheme.colorScheme.surfaceContainer,
                        RoundedCornerShape(12.dp),
                    )
                    .padding(8.dp)
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    val tint = MaterialTheme.colorScheme.onSurfaceVariant
                    Canvas(
                        Modifier.size(48.dp)
                            .semantics { contentDescription = "Déplacer $label" }
                            .onGloballyPositioned { handleTop = it.boundsInRoot().top }
                            .pointerInput(ids, enabled) {
                                if (!enabled || ids.size < 2) return@pointerInput
                                detectDragGestures(
                                    onDragStart = { offset ->
                                        dragged = id
                                        target = id
                                        pointerY = handleTop + offset.y
                                    },
                                    onDragCancel = {
                                        dragged = null
                                        target = null
                                    },
                                    onDragEnd = {
                                        val from = ids.indexOf(id)
                                        val to = ids.indexOf(target)
                                        dragged = null
                                        target = null
                                        if (from >= 0 && to >= 0 && from != to) {
                                            val order = ids.toMutableList()
                                            order.add(to, order.removeAt(from))
                                            latestReorder(order)
                                        }
                                    },
                                ) { event, amount ->
                                    event.consume()
                                    pointerY += amount.y
                                    target = ids.find { candidate ->
                                        val row = bounds[candidate]
                                        row != null && pointerY >= row.top && pointerY < row.bottom
                                    }
                                }
                            }
                    ) {
                        for (x in listOf(-3.dp.toPx(), 3.dp.toPx())) {
                            for (y in listOf(-6.dp.toPx(), 0f, 6.dp.toPx())) {
                                drawCircle(tint, 1.5.dp.toPx(), center + Offset(x, y))
                            }
                        }
                    }
                    Text("${index + 1}. $label", Modifier.weight(1f))
                }
                Row {
                    TextButton(
                        enabled = enabled && index > 0,
                        onClick = {
                            val order = ids.toMutableList()
                            order.add(index - 1, order.removeAt(index))
                            reorder(order)
                        },
                        modifier = Modifier.semantics { contentDescription = "Monter $label" },
                    ) {
                        Text("Monter")
                    }
                    TextButton(
                        enabled = enabled && index < ids.lastIndex,
                        onClick = {
                            val order = ids.toMutableList()
                            order.add(index + 1, order.removeAt(index))
                            reorder(order)
                        },
                        modifier = Modifier.semantics { contentDescription = "Descendre $label" },
                    ) {
                        Text("Descendre")
                    }
                    TextButton(enabled = enabled, onClick = { remove(id) }) {
                        Text("Retirer la priorité")
                    }
                }
            }
        }
    }
}
