package build.cairn.app.data

import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NodesTest {
    private val node =
        ExecutionNode(
            id = "67096815-500d-5d8e-94fc-396f75f5aedf",
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
            agents =
                listOf(
                    NodeAgent(
                        id = "a",
                        name = "Main",
                        allNodes = false,
                    )
                ),
            createdAt = 0,
            revision = "fixture",
            limits = NodeResources(cpu = 1, memoryMiB = 256, diskMiB = 128),
            storage = NodeStoragePolicy(),
            staleDisks = StaleDisks(count = 0, diskMiB = 0),
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
    fun diskAccountingIsOptionalInTheCurrentInventory() {
        val decoded =
            wireJson.decodeFromString<ExecutionNode>(
                nodeWireFixture(
                    """{"id":"67096815-500d-5d8e-94fc-396f75f5aedf","name":"Desktop","status":"online","diskAccounting":{"capturedAt":10,"localBytes":4096,"unusedGrantedBytes":8192,"hostFreeBytes":16384,"hostReserveBytes":1024,"poolReserveBytes":512,"reclaimableBytes":2048,"unsynchronizedBytes":1024}}"""
                )
            )
        assertEquals(2048L, decoded.diskAccounting?.reclaimableBytes)
        assertEquals(1024L, decoded.diskAccounting?.unsynchronizedBytes)
        assertNull(node.diskAccounting)
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

        // Native nodes need ublk rather than KVM, and run under their own service.
        val native =
            node.copy(
                nativeEnabled = true,
                nativeAccounts = listOf("p"),
                capabilities =
                    node.capabilities.copy(backend = NATIVE_BACKEND, kvm = false, ublk = true),
            )
        assertEquals(emptyList<String>(), nodeDiagnostics(native))
        assertEquals(
            listOf(
                "ublk indisponible : cette node a besoin d’un runtime natif à jour et du module noyau ublk_drv (/dev/ublk-control) pour monter les disques de conversation stockés sur S3."
            ),
            nodeDiagnostics(native.copy(capabilities = native.capabilities.copy(ublk = false))),
        )
        assertTrue(
            nodeDiagnostics(native.copy(status = "offline"), 600_000)
                .first()
                .contains("le service cairn-native-node tourne")
        )
    }

    @Test
    fun exclusiveGrantsAreNeverSentFromTheNode() {
        val oldNode =
            wireJson.decodeFromString<ExecutionNode>(
                nodeWireFixture(
                    """{"id":"67096815-500d-5d8e-94fc-396f75f5aedf","name":"Desktop","status":"online"}"""
                )
            )
        assertTrue(oldNode.exclusiveAgents.isEmpty())
        assertFalse(oldNode.exclusiveReservations)
        assertNull(oldNode.exclusiveReservation)

        // Omitted, the server keeps the grants saved on each agent.
        val configuration =
            NodeConfiguration(
                name = "Desktop",
                accepting = true,
                tags = emptyList(),
                limits =
                    NodeResources(
                        cpu = 1,
                        memoryMiB = 128,
                        diskMiB = 128,
                    ),
                revision = "fixture",
                nativeAccounts = null,
                slots = 4,
            )
        assertFalse("exclusiveAgents" in wireJson.encodeToJsonElement(configuration).jsonObject)
    }

    @Test
    fun exclusiveReservationSurvivesNodeAndRunSerialization() {
        val reservation =
            ExclusiveReservation(
                id = "reservation",
                nodeId = "b40903f9-0487-53d0-a309-c589f0cae405",
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
        val run =
            Run(
                id = "neighbor",
                status = "running",
                suspendedByExclusive = reservation,
                taskId = "",
            )
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
