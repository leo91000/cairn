package build.cairn.app.data

import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NodesTest {
    private val node =
        ExecutionNode(
            id = "n",
            name = "Desktop",
            accepting = true,
            status = "online",
            executionReady = true,
            lastSeen = 0,
            capabilities =
                NodeCapabilities(
                    cpu = 8,
                    memoryMiB = 16384,
                    diskMiB = 65536,
                    os = "linux",
                    arch = "x86_64",
                    kvm = true,
                ),
            agents = listOf(NodeAgent("a", "Main")),
        )

    @Test
    fun sizesAndAgesAreReadable() {
        assertEquals("512 Mio", formatMiB(512))
        assertEquals("32 Gio", formatMiB(32768))
        assertEquals("15,5 Gio", formatMiB(15872))
        assertEquals("0 o", formatBytes(0))
        assertEquals("1 o", formatBytes(1))
        assertEquals("1,5 Kio", formatBytes(1536))
        assertEquals("182,5 Mio", formatBytes(191392768))
        assertEquals("il y a moins d’une minute", relativeAge(0, 30_000))
        assertEquals("il y a 3 min", relativeAge(0, 180_000))
        assertEquals("il y a 5 h", relativeAge(0, 5 * 3_600_000))
    }

    @Test
    fun diagnosticsExplainWhyANodeTakesNoWork() {
        assertEquals(emptyList<String>(), nodeDiagnostics(node))
        assertTrue(
            nodeDiagnostics(node.copy(executionReady = false, budgetError = "RAM trop basse"))
                .first()
                .contains("RAM trop basse")
        )
        assertEquals(
            listOf("Aucun agent ne peut encore utiliser cette machine."),
            nodeDiagnostics(node.copy(agents = emptyList())),
        )
        assertTrue(
            nodeDiagnostics(node.copy(status = "offline"), 600_000)
                .first()
                .startsWith("Aucun contact depuis 10 min")
        )
    }

    @Test
    fun exclusivePermissionsRemainExplicitAndLegacyConfigurationPreservesThem() {
        val oldNode =
            wireJson.decodeFromString<ExecutionNode>(
                """{"id":"n","name":"Desktop","status":"online"}"""
            )
        assertTrue(oldNode.exclusiveAgents.isEmpty())
        assertFalse(oldNode.exclusiveReservations)
        assertNull(oldNode.exclusiveReservation)

        val configuration = NodeConfiguration("Desktop", true, emptyList(), NodeResources())
        assertFalse("exclusiveAgents" in wireJson.encodeToJsonElement(configuration).jsonObject)
        val granted =
            wireJson
                .encodeToJsonElement(configuration.copy(exclusiveAgents = listOf("agent")))
                .jsonObject
        assertEquals("agent", granted["exclusiveAgents"]!!.jsonArray.single().jsonPrimitive.content)
        val revoked =
            wireJson
                .encodeToJsonElement(configuration.copy(exclusiveAgents = emptyList()))
                .jsonObject
        assertTrue(revoked["exclusiveAgents"]!!.jsonArray.isEmpty())
    }

    @Test
    fun exclusiveReservationSurvivesNodeAndRunSerialization() {
        val reservation =
            ExclusiveReservation(
                id = "reservation",
                nodeId = "node",
                ownerRunId = "owner",
                reason = "Compilation",
                state = "acquired",
                createdAt = 100,
                expiresAt = 1000,
                renewalPending = true,
                pausedRunIds = listOf("neighbor"),
                minimumAvailableMemoryMiB = 4096,
            )
        val encodedNode =
            wireJson.encodeToString(
                ExecutionNode.serializer(),
                node.copy(exclusiveReservation = reservation),
            )
        assertEquals(
            reservation,
            wireJson.decodeFromString<ExecutionNode>(encodedNode).exclusiveReservation,
        )
        val run = Run("neighbor", status = "running", suspendedByExclusive = reservation)
        assertEquals(
            reservation,
            wireJson
                .decodeFromString<Run>(wireJson.encodeToString(Run.serializer(), run))
                .suspendedByExclusive,
        )
        val chat = Chat("chat", status = "running", suspendedByExclusive = reservation)
        val decodedChat =
            wireJson.decodeFromString<Chat>(wireJson.encodeToString(Chat.serializer(), chat))
        assertEquals(reservation, decodedChat.suspendedByExclusive)
        assertTrue(decodedChat.exclusiveSuspended)
        assertFalse(decodedChat.copy(status = "failed").exclusiveSuspended)
        assertEquals("Exclusivité acquise", exclusiveReservationState(reservation.state))
        assertEquals("future-state", exclusiveReservationState("future-state"))
    }
}
