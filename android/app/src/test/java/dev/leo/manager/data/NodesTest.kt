package dev.leo.manager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NodesTest {
    private val node = ExecutionNode(
        id = "n", name = "Desktop", accepting = true, status = "online", executionReady = true, lastSeen = 0,
        capabilities = NodeCapabilities(cpu = 8, memoryMiB = 16384, diskMiB = 65536, os = "linux", arch = "x86_64", kvm = true),
        agents = listOf(NodeAgent("a", "Main")),
    )

    @Test fun sizesAndAgesAreReadable() {
        assertEquals("512 Mio", formatMiB(512))
        assertEquals("32 Gio", formatMiB(32768))
        assertEquals("15,5 Gio", formatMiB(15872))
        assertEquals("il y a moins d’une minute", relativeAge(0, 30_000))
        assertEquals("il y a 3 min", relativeAge(0, 180_000))
        assertEquals("il y a 5 h", relativeAge(0, 5 * 3_600_000))
    }

    @Test fun diagnosticsExplainWhyANodeTakesNoWork() {
        assertEquals(emptyList<String>(), nodeDiagnostics(node))
        assertEquals(listOf("Aucun agent ne peut encore utiliser cette machine."), nodeDiagnostics(node.copy(agents = emptyList())))
        assertTrue(nodeDiagnostics(node.copy(status = "offline"), 600_000).first().startsWith("Aucun contact depuis 10 min"))
    }
}
