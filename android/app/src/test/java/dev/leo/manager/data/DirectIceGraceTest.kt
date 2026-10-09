package dev.leo.manager.data

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class DirectIceGraceTest {
    @Test
    fun `disconnected ICE can recover during five seconds without losing transport`() = runTest {
        var failed = false
        val ice = DirectIceGrace(backgroundScope) { failed = true }
        ice.changed(disconnected = true)
        runCurrent()
        advanceTimeBy(4999)
        assertFalse(failed)
        ice.changed(disconnected = false)
        advanceTimeBy(10000)
        runCurrent()
        assertFalse(failed)
    }

    @Test
    fun `prolonged ICE disconnection fails at five seconds`() = runTest {
        var failed = false
        val ice = DirectIceGrace(backgroundScope) { failed = true }
        ice.changed(disconnected = true)
        runCurrent()
        advanceTimeBy(4999)
        assertFalse(failed)
        advanceTimeBy(1)
        runCurrent()
        assertTrue(failed)
    }

    @Test
    fun `failed or closed ICE bypasses the grace and cancels its old timer`() = runTest {
        var failures = 0
        val ice = DirectIceGrace(backgroundScope) { failures++ }
        ice.changed(disconnected = true)
        runCurrent()
        advanceTimeBy(1000)
        ice.changed(disconnected = false, terminal = true)
        assertEquals(1, failures)
        advanceTimeBy(10000)
        runCurrent()
        assertEquals(1, failures)
    }
}
