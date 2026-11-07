package build.cairn.app.ui

import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.dp
import org.junit.Assert.*
import org.junit.Test

class DiagramPreviewTest {
    @Test
    fun `preview sizes fit Compose constraints across display densities`() {
        for ((width, height) in listOf(30503.0 to 4646.0, 4646.0 to 30503.0, 32700.0 to 32700.0)) {
            for (scale in listOf(1f, 2f, 2.625f, 3f, 4f, 6f)) {
                val density = Density(scale)
                val ratio = diagramPreviewRatio(width, height, 360.0, density)
                assertTrue(ratio > 0 && ratio <= 1)
                val pixels =
                    with(density) {
                        (width * ratio).dp.roundToPx() to (height * ratio).dp.roundToPx()
                    }
                val constraints = Constraints.fixed(pixels.first, pixels.second)
                assertEquals(pixels.first, constraints.maxWidth)
                assertEquals(pixels.second, constraints.maxHeight)
            }
        }
    }

    @Test
    fun `representable previews retain their readable scale`() {
        assertEquals(1.0, diagramPreviewRatio(100.0, 100.0, 360.0, Density(3f)), 0.0)
        assertEquals(0.75, diagramPreviewRatio(800.0, 400.0, 320.0, Density(2f)), 0.0)
        assertEquals(0.75, diagramPreviewRatio(30000.0, 1000.0, 360.0, Density(3f)), 0.0)
    }
}
