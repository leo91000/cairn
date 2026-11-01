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
        assertEquals(
            NodeHealth(NodeHealthState.FULL, "Pleine", "Aucun slot disponible"),
            nodeHealth(ready.copy(availableSlots = 0, occupiedSlots = 1)),
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
        // The built-in runner is judged against server time, never against the device clock offset.
        val runner = ready.copy(local = true, status = "local", lastSeen = 0)
        val remote = ready.copy(id = "0d7c9a6e-3b8f-5b1e-9f3a-2c4d5e6f7a8b", lastSeen = 120_000)
        assertEquals(120_000L, fleetClock(listOf(runner, remote)))
        assertNull(fleetClock(listOf(runner)))
        assertEquals(
            NodeHealth(
                NodeHealthState.ATTENTION,
                "À vérifier",
                "Dernier rapport exploitable il y a 2 min",
            ),
            nodeHealth(runner, 0, 120_000),
        )
        assertTrue(runnerStale(runner, 120_000))
        assertTrue(nodeDiagnostics(runner, 0, 120_000).first().contains("depuis 2 min"))
        assertEquals(NodeHealthState.READY, nodeHealth(runner, 0, 30_000).state)
        assertEquals(
            "Dernier contact il y a 2 min",
            nodeHealth(ready.copy(status = "offline", lastSeen = 0), 3_600_000, 120_000).detail,
        )
        assertEquals(
            "Dernier contact il y a 1 min",
            nodeHealth(ready.copy(status = "offline", lastSeen = 0), 0, 50_000).detail,
        )
        // A device clock far ahead of the server does not make a refreshed runner look stale.
        assertEquals(
            NodeHealthState.READY,
            nodeHealth(runner.copy(lastSeen = 120_000), 900_000, 125_000).state,
        )
        assertEquals(NodeHealthState.READY, nodeHealth(runner, 900_000).state)
        // A lone runner goes stale when its contact stays unchanged for a minute of successful
        // listings; failed listings do not count.
        val alone = runner.copy(lastSeen = 1_000)
        val seen = observeRunner(listOf(alone), null, 3_600_000)!!
        assertEquals(RunnerObservation(1_000, 3_600_000, 3_600_000), seen)
        assertEquals(
            RunnerObservation(11_000, 3_610_000, 3_610_000, refreshed = true),
            observeRunner(listOf(runner.copy(lastSeen = 11_000)), seen, 3_610_000),
        )
        val later = observeRunner(listOf(alone), seen, 3_659_000)!!
        assertEquals(seen.copy(checkedAt = 3_659_000), later)
        assertEquals(NodeHealthState.READY, nodeHealth(alone, 3_659_000, null, later).state)
        assertEquals(NodeHealthState.READY, nodeHealth(alone, 9_999_999, null, later).state)
        assertEquals(
            NodeHealth(
                NodeHealthState.ATTENTION,
                "À vérifier",
                "Aucun rapport exploitable depuis plus d’une minute",
            ),
            nodeHealth(alone, 3_660_000, null, observeRunner(listOf(alone), later, 3_660_000)),
        )
        // A contact that changed while watched is server time: it dates other machines.
        val refreshedRunner = listOf(runner.copy(lastSeen = 11_000))
        val watched = observeRunner(refreshedRunner, seen, 3_610_000)
        assertEquals(
            71_000L,
            fleetClock(refreshedRunner, observeRunner(refreshedRunner, watched, 3_670_000)),
        )
        // Free slots follow the runtime pool, as placement does.
        assertEquals(
            1L,
            freeSlots(
                ready.copy(slots = 4, runtimeSlots = 2, occupiedSlots = 1, availableSlots = 3)
            ),
        )
        assertEquals(
            NodeHealth(NodeHealthState.FULL, "Pleine", "Les 2 slots sont occupés"),
            nodeHealth(
                ready.copy(slots = 4, runtimeSlots = 2, occupiedSlots = 2, availableSlots = 2)
            ),
        )
        // A runner already stale when the screen opened does not shorten other ages.
        val stale = observeRunner(listOf(runner), null, 7_200_000)
        assertEquals(
            "Dernier contact il y a 2 h",
            nodeHealth(ready.copy(status = "offline", lastSeen = 0), 7_200_000, null, stale).detail,
        )
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
    fun lastingUnreadyAndFleetSummaryMatchTheWeb() {
        val ready = node.copy(slots = 4, availableSlots = 3, occupiedSlots = 1)
        val waiting = ready.copy(executionReady = false)
        val first = observeUnready(listOf(waiting), emptyMap(), 1_000)
        assertEquals(mapOf(waiting.id to UnreadyObservation(1_000, 1_000)), first)
        assertEquals(NodeHealthState.UPDATING, nodeHealth(waiting, 0, null, null, first).state)
        val later = observeUnready(listOf(waiting), first, 1_000 + LASTING_UNREADY_MS)
        assertEquals(
            NodeHealth(
                NodeHealthState.ATTENTION,
                "À vérifier",
                "Pas prête depuis plus de 5 minutes",
            ),
            nodeHealth(waiting, 0, null, null, later),
        )
        assertTrue(observeUnready(listOf(ready), later, 400_000).isEmpty())
        assertTrue(
            observeUnready(listOf(waiting.copy(budgetError = "cgroup")), later, 400_000).isEmpty()
        )
        // Time spent offline does not count: the grace period starts at "Pas prête".
        val disconnected = waiting.copy(status = "offline")
        val away =
            observeUnready(
                listOf(disconnected),
                observeUnready(listOf(disconnected), emptyMap(), 0),
                600_000,
            )
        assertTrue(away.isEmpty())
        assertEquals(
            NodeHealthState.UPDATING,
            nodeHealth(
                    waiting,
                    0,
                    null,
                    null,
                    observeUnready(listOf(waiting), away, 610_000),
                )
                .state,
        )
        assertEquals(
            NodeHealth(NodeHealthState.FULL, "Pleine", "Le runtime n’offre aucun slot"),
            nodeHealth(
                ready.copy(slots = 2, runtimeSlots = 0, occupiedSlots = 0, availableSlots = 2)
            ),
        )
        // Time changing privileges does not count either.
        val changing =
            waiting.copy(
                capabilities =
                    waiting.capabilities.copy(
                        backend = NATIVE_BACKEND,
                        ublk = true,
                        nativeModeProtocol = 1,
                    ),
                nativeEnabled = true,
                nativeAccounts = listOf("person"),
                nativeModeStatus = "pending",
            )
        assertTrue(observeUnready(listOf(changing), emptyMap(), 0).isEmpty())
        // Deliberate choices: native execution switched off, an unused built-in runner.
        val switchedOff =
            changing.copy(nativeEnabled = false, nativeModeStatus = null, executionReady = true)
        assertEquals(
            NodeHealth(NodeHealthState.PAUSED, "En pause", "Exécution native désactivée"),
            nodeHealth(switchedOff),
        )
        val unused =
            ready.copy(local = true, status = "local", lastSeen = null, agents = emptyList())
        assertEquals(NodeHealthState.SETUP, nodeHealth(unused).state)
        assertEquals(0, fleetSummary(listOf(unused, switchedOff)).attention)
        // Revoked machines leave the summary.
        assertEquals(0, fleetSummary(listOf(ready.copy(revoked = true))).machines)
        assertEquals(3L, slotCapacity(ready.copy(slots = null, runtimeSlots = 3)))
        assertEquals(4L, slotCapacity(ready.copy(slots = null, runtimeSlots = null)))

        val runner =
            ready.copy(
                id = "8d0f1c3e-2a4b-5c6d-8e7f-9a0b1c2d3e4f",
                local = true,
                status = "local",
                lastSeen = 0,
                runtimeSlots = 2,
                occupiedSlots = 3,
                availableSlots = 1,
            )
        val online = ready.copy(lastSeen = 120_000)
        val setup =
            ready.copy(
                id = "1a2b3c4d-5e6f-5a7b-8c9d-0e1f2a3b4c5d",
                lastSeen = 120_000,
                agents = emptyList(),
            )
        val offline =
            ready.copy(
                id = "2b3c4d5e-6f7a-5b8c-9d0e-1f2a3b4c5d6e",
                status = "offline",
                lastSeen = 0,
            )
        assertEquals(
            FleetSummary(machines = 4, connected = 2, usedSlots = 2, totalSlots = 8, attention = 3),
            fleetSummary(listOf(runner, online, setup, offline), 0, 120_000),
        )
        assertEquals(
            FleetSummary(machines = 2, connected = 2, usedSlots = 3, totalSlots = 6, attention = 0),
            fleetSummary(listOf(runner.copy(lastSeen = 120_000), online), 0, 120_000),
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
        val chat = Chat(id = "chat", status = "running", suspendedByExclusive = reservation)
        val decodedChat =
            wireJson.decodeFromString<Chat>(wireJson.encodeToString(Chat.serializer(), chat))
        assertEquals(reservation, decodedChat.suspendedByExclusive)
        assertTrue(decodedChat.exclusiveSuspended)
        assertFalse(decodedChat.copy(status = "failed").exclusiveSuspended)
        assertEquals("Exclusivité acquise", exclusiveReservationState(reservation.state))
        assertEquals("future-state", exclusiveReservationState("future-state"))
    }
}
