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
    fun healthNamesTheFirstBlocker() {
        val ready = node.copy(slots = 4, availableSlots = 3, occupiedSlots = 1)
        assertEquals(
            NodeHealth(NodeHealthState.READY, "Prête", "1 en cours · 3 slots libres sur 4"),
            nodeHealth(ready),
        )
        assertEquals(
            NodeHealthState.FULL,
            nodeHealth(ready.copy(availableSlots = 0, occupiedSlots = 4)).state,
        )
        assertEquals(NodeHealthState.REVOKED, nodeHealth(ready.copy(revoked = true)).state)
        assertEquals(
            NodeHealth(NodeHealthState.OFFLINE, "Hors ligne", "Dernier contact il y a 3 h"),
            nodeHealth(ready.copy(status = "offline", agents = emptyList()), 3 * 3_600_000),
        )
        assertEquals(
            "Jamais connectée",
            nodeHealth(ready.copy(status = "offline", lastSeen = null)).detail,
        )
        assertEquals(
            NodeHealthState.ATTENTION,
            nodeHealth(ready.copy(capabilities = ready.capabilities.copy(kvm = false))).state,
        )
        assertEquals(
            NodeHealthState.UPDATING,
            nodeHealth(ready.copy(maintenance = "draining", executionReady = false)).state,
        )
        // A saved budget turns readiness off until the next heartbeat applies it: not a fault.
        assertEquals(
            NodeHealth(
                NodeHealthState.UPDATING,
                "Pas prête",
                "Le runtime n’est pas encore prêt",
            ),
            nodeHealth(ready.copy(executionReady = false)),
        )
        assertEquals(
            NodeHealth(NodeHealthState.ATTENTION, "À vérifier", "La mise à jour a échoué"),
            nodeHealth(ready.copy(executionReady = false, updateError = "image")),
        )
        assertEquals(
            "Aucun agent ne peut encore utiliser cette machine",
            nodeHealth(ready.copy(agents = emptyList(), accepting = false)).detail,
        )
        assertEquals(
            NodeHealthState.PAUSED,
            nodeHealth(ready.copy(accepting = false, pressure = "memory")).state,
        )
        assertEquals(
            NodeHealth(NodeHealthState.BUSY, "Chargée", "RAM partagée sous pression"),
            nodeHealth(ready.copy(pressure = "memory")),
        )
        assertEquals(
            NodeHealth(NodeHealthState.RESERVED, "Réservée", "Benchmark GPU"),
            nodeHealth(
                ready.copy(
                    pressure = "memory",
                    exclusiveReservation =
                        build.cairn.boundary.ExclusiveReservation(
                            id = "reservation",
                            nodeId = ready.id,
                            ownerRunId = "run",
                            reason = "Benchmark GPU",
                            state = "acquired",
                            createdAt = 0,
                            waitUntil = 0,
                        ),
                )
            ),
        )
        assertEquals(
            "Espace disque partagé insuffisant",
            nodeHealth(ready.copy(pressure = "disk")).detail,
        )
        // The built-in runner is judged against the server clock seen in the fleet.
        val runner = ready.copy(local = true, status = "local", lastSeen = 0)
        val remote = ready.copy(id = "0d7c9a6e-3b8f-5b1e-9f3a-2c4d5e6f7a8b", lastSeen = 120_000)
        assertEquals(120_000L, fleetClock(listOf(runner, remote)))
        assertNull(fleetClock(listOf(runner)))
        assertEquals(
            NodeHealth(
                NodeHealthState.OFFLINE,
                "Hors ligne",
                "Injoignable · dernier contact il y a 2 min",
            ),
            nodeHealth(runner, 0, 120_000),
        )
        assertEquals(NodeHealthState.READY, nodeHealth(runner, 0, 30_000).state)
        assertEquals(
            NodeHealth(
                NodeHealthState.ATTENTION,
                "À vérifier",
                "Pas prêt depuis son dernier contact il y a 2 min",
            ),
            nodeHealth(runner.copy(executionReady = false), 0, 120_000),
        )
        assertEquals(
            "Dernier contact il y a 2 min",
            nodeHealth(ready.copy(status = "offline", lastSeen = 0), 3_600_000, 120_000).detail,
        )
        // A device clock far ahead of the server does not make a refreshed runner look stale.
        assertEquals(
            NodeHealthState.READY,
            nodeHealth(runner.copy(lastSeen = 120_000), 900_000, 125_000).state,
        )
        assertEquals(NodeHealthState.READY, nodeHealth(runner, 900_000).state)
        val native =
            ready.copy(
                capabilities =
                    ready.capabilities.copy(
                        backend = NATIVE_BACKEND,
                        ublk = true,
                        nativeModeProtocol = 1,
                    )
            )
        assertEquals(NodeHealthState.SETUP, nodeHealth(native).state)
        val enabled = native.copy(nativeEnabled = true, nativeAccounts = listOf("person"))
        assertEquals(
            "Mettez à jour le superviseur pour confirmer les privilèges",
            nodeHealth(
                    enabled.copy(
                        capabilities = enabled.capabilities.copy(nativeModeProtocol = 0),
                        nativeModeStatus = "pending",
                    )
                )
                .detail,
        )
        assertEquals(
            "Un agent demande Natif administrateur",
            nodeHealth(
                    enabled.copy(
                        nativeModeProposal =
                            build.cairn.contracts.NativeModeProposal(
                                mode = "administrator",
                                previewId = "preview",
                                status = "pending",
                            )
                    )
                )
                .detail,
        )
        assertEquals(
            NodeHealthState.UPDATING,
            nodeHealth(enabled.copy(nativeModeStatus = "pending", executionReady = false)).state,
        )
        assertEquals(
            "Le changement de privilèges a échoué",
            nodeHealth(enabled.copy(nativeModeStatus = "failed", executionReady = false)).detail,
        )
        assertEquals(
            NodeHealth(
                NodeHealthState.ATTENTION,
                "À vérifier",
                "La préparation de la mise à jour a échoué",
            ),
            nodeHealth(ready.copy(maintenance = "draining", maintenanceError = "disque plein")),
        )
        assertEquals(
            NodeHealthState.READY,
            nodeHealth(native.copy(nativeEnabled = true, nativeAccounts = listOf("person"))).state,
        )
    }

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
