package build.cairn.app.data

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.ProcessLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import java.io.IOException
import java.net.InetAddress
import java.net.UnknownHostException
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.selects.select
import kotlinx.serialization.json.*
import okhttp3.Call
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import org.webrtc.*

/** One authorized installation peer. Lifecycle and network changes never stop agent runs. */
internal class NativeDirect(private val api: CairnApi, context: Context) {
    companion object {
        private val initialization = Any()
        private var initialized = false

        // Match the web client's numeric destination policy. The Rust verifier remains
        // authoritative; validate the literal before InetAddress so no DNS lookup can occur.
        private fun usableCandidate(candidate: String): Boolean {
            if (candidate.isEmpty()) return true
            val address = candidate.trim().split(Regex("\\s+")).getOrNull(4) ?: return false
            if (Regex("\\d+\\.\\d+\\.\\d+\\.\\d+").matches(address)) {
                val octets = address.split('.').map { it.toIntOrNull() ?: return false }
                return octets.all { it in 0..255 } &&
                    octets[0] != 127 &&
                    octets.any { it != 0 } &&
                    !(octets[0] == 169 && octets[1] == 254) &&
                    octets[0] !in 224..239 &&
                    !octets.all { it == 255 }
            }
            if (!address.contains(':') || !Regex("[0-9a-fA-F:.]+").matches(address)) return false
            val literal = "http://[$address]/".toHttpUrlOrNull()?.host ?: return false
            val ip =
                try {
                    InetAddress.getByName(literal)
                } catch (_: UnknownHostException) {
                    return false
                }
            return !ip.isAnyLocalAddress &&
                !ip.isLoopbackAddress &&
                !ip.isLinkLocalAddress &&
                !ip.isMulticastAddress &&
                !ip.address.all { it.toInt() and 255 == 255 }
        }

        private fun usableSdp(sdp: String): String =
            sdp.lineSequence()
                .filter { !it.startsWith("a=candidate:") || usableCandidate(it.removePrefix("a=")) }
                .joinToString("\r\n")
    }

    private class Unavailable : IOException("Direct indisponible")

    private val denied = AtomicBoolean()
    private val generation = AtomicLong()
    private val unavailableAt = AtomicLong(-1)
    private val retry = DirectRetry()
    private val stopped = AtomicBoolean()
    private val application = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val manager = application.getSystemService(ConnectivityManager::class.java)
    private val active = AtomicReference<Attempt?>()
    private val changed = Channel<Unit>(Channel.CONFLATED)
    @Volatile private var network = manager.activeNetwork
    private val restart = AtomicBoolean()
    private val callback =
        object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(next: Network) {
                if (next != network) networkChanged(next)
            }

            override fun onLost(previous: Network) {
                if (previous == network) networkChanged(null)
            }
        }

    private fun networkChanged(next: Network?) {
        network = next
        synchronized(retry) {
            generation.incrementAndGet()
            retry.networkChanged()
        }
        restart.set(true)
        active.get()?.fail()
        changed.trySend(Unit)
    }

    init {
        scope.launch {
            try {
                synchronized(initialization) {
                    if (!initialized) {
                        PeerConnectionFactory.initialize(
                            PeerConnectionFactory.InitializationOptions.builder(application)
                                .setInjectableLogger({ _, _, _ -> }, Logging.Severity.LS_NONE)
                                .createInitializationOptions()
                        )
                        initialized = true
                    }
                }
            } catch (_: LinkageError) {
                // An unsupported native ABI keeps the fully functional relay.
                return@launch
            }
            ProcessLifecycleOwner.get().lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                if (manager.activeNetwork != network) networkChanged(manager.activeNetwork)
                manager.registerDefaultNetworkCallback(callback)
                try {
                    while (isActive) {
                        val attemptGeneration = generation.get()
                        if (denied.get() || unavailableAt.get() == attemptGeneration) {
                            changed.receive()
                            continue
                        }
                        val remaining = retry.remaining()
                        if (remaining > 0) {
                            withTimeoutOrNull(remaining) { changed.receive() }
                            continue
                        }
                        val attempt = Attempt(attemptGeneration)
                        active.set(attempt)
                        var attemptFailed = false
                        try {
                            attempt.connect(restart.getAndSet(false))
                            attempt.failed.await()
                            attemptFailed = true
                        } catch (error: CancellationException) {
                            throw error
                        } catch (error: Unavailable) {
                            unavailableAt.set(attemptGeneration)
                        } catch (error: Exception) {
                            attemptFailed = true
                            rememberDenial(error)
                            // Direct is optional; the relay reports installation/session failures.
                        } finally {
                            // Persist failure timing before lifecycle cancellation can interrupt
                            // cleanup. A deliberate background close does not count as a failure.
                            synchronized(retry) {
                                if (
                                    (attemptFailed || attempt.failed.isCompleted) &&
                                        generation.get() == attemptGeneration &&
                                        !denied.get() &&
                                        unavailableAt.get() != attemptGeneration
                                )
                                    retry.failed()
                            }
                            active.compareAndSet(attempt, null)
                            withContext(NonCancellable) { attempt.close() }
                        }
                    }
                } finally {
                    manager.unregisterNetworkCallback(callback)
                }
            }
        }
    }

    fun close() {
        if (!stopped.compareAndSet(false, true)) return
        active.get()?.fail()
        scope.cancel()
    }

    private fun rememberDenial(error: Exception) {
        if (error is ApiException && error.status in setOf(401, 403)) denied.set(true)
    }

    private inner class Attempt(private val attemptGeneration: Long) {
        val failed = CompletableDeferred<Unit>()
        private val opened = CompletableDeferred<Unit>()
        private val expires = AtomicLong()
        private val candidates = Channel<JsonObject>(16)
        private val workers =
            CoroutineScope(SupervisorJob(scope.coroutineContext[Job]) + Dispatchers.IO)
        private val writable = Channel<Unit>(Channel.CONFLATED)
        private val dataLock = Any()
        private val iceGrace = DirectIceGrace(workers) { fail() }
        private val events = AtomicReference<Call?>()
        private var factory: PeerConnectionFactory? = null
        private var peer: PeerConnection? = null
        private val data = AtomicReference<DataChannel?>()
        @Volatile private var traffic: DirectChannel? = null
        private var connection = ""
        private var fingerprint = ""

        fun fail(error: Exception? = null) {
            // Renewal unavailability belongs to this network generation, even if a newer
            // default network was observed while the HTTP request was in flight.
            if (error is Unavailable) unavailableAt.set(attemptGeneration)
            error?.let(::rememberDenial)
            traffic?.close()
            events.get()?.cancel()
            failed.complete(Unit)
        }

        suspend fun connect(iceRestart: Boolean) {
            val configuration =
                PeerConnection.RTCConfiguration(emptyList()).apply {
                    tcpCandidatePolicy = PeerConnection.TcpCandidatePolicy.DISABLED
                    continualGatheringPolicy =
                        PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
                }
            factory = PeerConnectionFactory.builder().createPeerConnectionFactory()
            peer =
                factory!!.createPeerConnection(
                    configuration,
                    object : PeerConnection.Observer {
                        override fun onSignalingChange(state: PeerConnection.SignalingState) {}

                        override fun onIceConnectionChange(
                            state: PeerConnection.IceConnectionState
                        ) {
                            iceGrace.changed(
                                disconnected =
                                    state == PeerConnection.IceConnectionState.DISCONNECTED,
                                terminal =
                                    state == PeerConnection.IceConnectionState.FAILED ||
                                        state == PeerConnection.IceConnectionState.CLOSED,
                            )
                        }

                        override fun onIceConnectionReceivingChange(receiving: Boolean) {}

                        override fun onIceGatheringChange(
                            state: PeerConnection.IceGatheringState
                        ) {}

                        override fun onIceCandidate(candidate: IceCandidate) {
                            if (!usableCandidate(candidate.sdp)) return
                            val value = buildJsonObject {
                                put("kind", "candidate")
                                put("candidate", candidate.sdp)
                                put("sdp_mid", candidate.sdpMid)
                                put("sdp_m_line_index", candidate.sdpMLineIndex)
                            }
                            if (!candidates.trySend(value).isSuccess) fail()
                        }

                        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) {}

                        override fun onAddStream(stream: MediaStream) {
                            fail()
                        }

                        override fun onRemoveStream(stream: MediaStream) {}

                        override fun onDataChannel(channel: DataChannel) {
                            fail()
                        }

                        override fun onRenegotiationNeeded() {}
                    },
                ) ?: throw IOException("Pair direct indisponible")
            data.set(peer!!.createDataChannel("cairn.v4", DataChannel.Init()))
            traffic =
                DirectChannel(
                    { packet, canceled, deadline -> send(packet, canceled, deadline) },
                    { failed.complete(Unit) },
                    {
                        !stopped.get() &&
                            !failed.isCompleted &&
                            System.currentTimeMillis() < expires.get()
                    },
                )
            data
                .get()!!
                .registerObserver(
                    object : DataChannel.Observer {
                        override fun onBufferedAmountChange(previous: Long) {
                            writable.trySend(Unit)
                        }

                        override fun onStateChange() {
                            // Native callbacks must never wait on the lock used around JNI
                            // writes/disposal.
                            workers.launch {
                                val state = synchronized(dataLock) { data.get()?.state() }
                                when (state) {
                                    DataChannel.State.OPEN -> {
                                        if (
                                            !failed.isCompleted &&
                                                System.currentTimeMillis() < expires.get()
                                        ) {
                                            retry.connected()
                                            api.transport.attach(traffic!!)
                                            opened.complete(Unit)
                                        } else fail()
                                    }
                                    DataChannel.State.CLOSED,
                                    DataChannel.State.CLOSING -> fail()
                                    else -> {}
                                }
                            }
                        }

                        override fun onMessage(buffer: DataChannel.Buffer) {
                            val size = buffer.data.remaining()
                            if (
                                !buffer.binary ||
                                    size !in DirectChannel.HEADER..DirectChannel.MAX_PACKET
                            ) {
                                fail()
                                return
                            }
                            // Copy native memory before callback return. The bounded worker queue
                            // waits under pressure; ordered assembly/JSON/base64 runs off JNI.
                            val packet = ByteArray(size)
                            buffer.data.get(packet)
                            traffic?.receive(packet)
                        }
                    }
                )
            if (iceRestart) peer!!.restartIce()
            val constraints =
                MediaConstraints().apply {
                    if (iceRestart)
                        mandatory.add(MediaConstraints.KeyValuePair("IceRestart", "true"))
                }
            val offer = requireNotNull(description { peer!!.createOffer(it, constraints) })
            val sdp =
                usableSdp(offer.description)
                    .lineSequence()
                    .filter { it.isNotEmpty() }
                    .joinToString("\r\n", postfix = "\r\n") { line ->
                        when {
                            line.startsWith("a=fingerprint:sha-256 ") ->
                                "a=fingerprint:sha-256 " +
                                    line.substringAfter("sha-256 ").uppercase()
                            // v4 negotiates trickle only. The optional SDK extension needs
                            // both peers' support; do not advertise it to the delivered verifier.
                            line == "a=ice-options:trickle renomination" -> "a=ice-options:trickle"
                            else -> line
                        }
                    }
            fingerprint =
                sdp.lineSequence()
                    .first { it.startsWith("a=fingerprint:") }
                    .removePrefix("a=fingerprint:")
            val grant = authorize("authorize")
            if (grant["available"]?.jsonPrimitive?.booleanOrNull != true) throw Unavailable()
            connection =
                grant["grant"]!!
                    .jsonObject["claims"]!!
                    .jsonObject["connection_id"]!!
                    .jsonPrimitive
                    .content
            updateGrant(grant)
            configuration.iceServers =
                grant["iceServers"]!!.jsonArray.flatMap { server ->
                    server.jsonObject["urls"]!!.jsonArray.map { url ->
                        val address = url.jsonPrimitive.content
                        require(address.startsWith("stun:") && address.length <= 2048)
                        PeerConnection.IceServer.builder(address).createIceServer()
                    }
                }
            if (!peer!!.setConfiguration(configuration))
                throw IOException("Configuration ICE refusée")
            workers.launch { readSignals() }
            // Acknowledge the offer before native trickle starts: the installation creates its peer
            // here.
            signal(
                buildJsonObject {
                    put("kind", "offer")
                    put("sdp", sdp)
                }
            )
            description {
                peer!!.setLocalDescription(
                    it,
                    SessionDescription(SessionDescription.Type.OFFER, sdp),
                )
            }
            workers.launch {
                try {
                    for (candidate in candidates) {
                        try {
                            signal(candidate)
                        } catch (error: ApiException) {
                            // One refused candidate does not invalidate the other ICE paths.
                            if (error.status != 400 && error.status != 429) throw error
                        }
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    fail(error)
                }
            }
            workers.launch {
                while (isActive && !failed.isCompleted) {
                    if (System.currentTimeMillis() >= expires.get()) {
                        fail()
                        break
                    }
                    traffic!!.checkDeadline()
                    delay(100)
                }
            }
            workers.launch {
                try {
                    while (isActive && !failed.isCompleted) {
                        delay(maxOf(10000L, expires.get() - System.currentTimeMillis() - 30000))
                        val previousDeadline = expires.get()
                        if (!updateGrant(authorize("$connection/renew"), previousDeadline)) break
                    }
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    fail(error)
                }
            }
            withTimeoutOrNull(30000) {
                select<Unit> {
                    opened.onAwait {}
                    failed.onAwait {}
                }
            } ?: fail()
        }

        private suspend fun authorize(path: String): JsonObject =
            wireJson
                .parseToJsonElement(
                    api.request(
                        "POST",
                        "/installations/${segment(api.installationId!!)}/direct/$path",
                        buildJsonObject {
                            put("fingerprint", fingerprint)
                            put("versions", buildJsonArray { add(4) })
                        },
                    )
                )
                .jsonObject

        private fun updateGrant(grant: JsonObject, previousDeadline: Long = 0): Boolean {
            if (grant["available"]?.jsonPrimitive?.booleanOrNull != true) throw Unavailable()
            val claims = grant["grant"]!!.jsonObject["claims"]!!.jsonObject
            require(
                claims["connection_id"]!!.jsonPrimitive.content == connection &&
                    claims["installation_id"]!!.jsonPrimitive.content == api.installationId &&
                    claims["fingerprint"]!!.jsonPrimitive.content == fingerprint
            )
            val deadline = claims["expires_at"]!!.jsonPrimitive.long * 1000
            require(deadline > System.currentTimeMillis())
            if (deadline <= previousDeadline) return false
            expires.set(deadline)
            return true
        }

        private suspend fun signal(value: JsonObject) {
            api.request(
                "POST",
                "/installations/${segment(api.installationId!!)}/direct/${segment(connection)}/signal",
                value,
            )
        }

        private suspend fun readSignals() {
            val call =
                api.streaming.newCall(
                    api.builder(
                            "/installations/${segment(api.installationId!!)}/direct/${segment(connection)}/events"
                        )
                        .header("Accept", "text/event-stream")
                        .get()
                        .build()
                )
            events.set(call)
            if (failed.isCompleted) call.cancel()
            try {
                call.execute().use { response ->
                    checkResponse(response)
                    val source = response.body.source()
                    var value = ""
                    var kind = ""
                    var answered = false
                    val waiting = mutableListOf<IceCandidate>()
                    while (!source.exhausted() && !failed.isCompleted) {
                        val line = source.readUtf8LineStrict(16384)
                        when {
                            line.startsWith("event:") -> kind = line.substringAfter(':').trim()
                            line.startsWith("data:") -> {
                                value += line.substringAfter(':').trim()
                                require(value.length <= 16384)
                            }
                            line.isEmpty() -> {
                                if (kind == "signal" && value.isNotEmpty()) {
                                    val incoming = wireJson.parseToJsonElement(value).jsonObject
                                    when (incoming["kind"]!!.jsonPrimitive.content) {
                                        "answer" -> {
                                            require(!answered)
                                            description {
                                                peer!!.setRemoteDescription(
                                                    it,
                                                    SessionDescription(
                                                        SessionDescription.Type.ANSWER,
                                                        usableSdp(
                                                            incoming["sdp"]!!.jsonPrimitive.content
                                                        ),
                                                    ),
                                                )
                                            }
                                            answered = true
                                            waiting.forEach { require(peer!!.addIceCandidate(it)) }
                                            waiting.clear()
                                        }
                                        "candidate" -> {
                                            val sdp = incoming["candidate"]!!.jsonPrimitive.content
                                            if (sdp.isNotEmpty() && usableCandidate(sdp)) {
                                                val candidate =
                                                    IceCandidate(
                                                        incoming["sdp_mid"]!!
                                                            .jsonPrimitive
                                                            .contentOrNull,
                                                        incoming["sdp_m_line_index"]!!
                                                            .jsonPrimitive
                                                            .int,
                                                        sdp,
                                                    )
                                                if (answered)
                                                    require(peer!!.addIceCandidate(candidate))
                                                else {
                                                    require(waiting.size < 16)
                                                    waiting.add(candidate)
                                                }
                                            }
                                        }
                                        else -> throw IOException("Signal direct inattendu")
                                    }
                                }
                                kind = ""
                                value = ""
                            }
                        }
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                rememberDenial(error)
            } finally {
                fail()
            }
        }

        private fun send(packet: ByteArray, canceled: () -> Boolean, deadline: Long) {
            while (true) {
                if (canceled()) throw IOException("Requête annulée")
                if (System.nanoTime() >= deadline) throw DirectRequestTimeout()
                val sent =
                    synchronized(dataLock) {
                        val channel = data.get() ?: throw DirectNotSent("Canal direct fermé")
                        try {
                            if (failed.isCompleted || channel.state() != DataChannel.State.OPEN)
                                throw DirectNotSent("Canal direct fermé")
                            // Leave native memory bounded even for an 8 MB application request.
                            channel.bufferedAmount() + packet.size <= 65536 &&
                                channel.send(DataChannel.Buffer(ByteBuffer.wrap(packet), true))
                        } catch (error: IllegalStateException) {
                            throw IOException("Canal direct fermé", error)
                        }
                    }
                if (sent) return
                runBlocking {
                    withTimeoutOrNull(20) { writable.receive() }
                }
            }
        }

        suspend fun close() {
            fail()
            workers.coroutineContext[Job]!!.cancelAndJoin()
            candidates.close()
            synchronized(dataLock) {
                data.getAndSet(null)?.let { channel ->
                    channel.unregisterObserver()
                    channel.close()
                    channel.dispose()
                }
            }
            peer?.close()
            peer?.dispose()
            factory?.dispose()
        }
    }

    private suspend fun description(start: (SdpObserver) -> Unit): SessionDescription? {
        val result = CompletableDeferred<SessionDescription?>()
        start(
            object : SdpObserver {
                override fun onCreateSuccess(description: SessionDescription) {
                    result.complete(description)
                }

                override fun onSetSuccess() {
                    result.complete(null)
                }

                override fun onCreateFailure(error: String) {
                    result.completeExceptionally(IOException("Négociation directe refusée"))
                }

                override fun onSetFailure(error: String) {
                    result.completeExceptionally(IOException("Négociation directe refusée"))
                }
            }
        )
        return result.await()
    }
}
